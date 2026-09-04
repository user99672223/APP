//! Local state in redb, every value sealed with XChaCha20-Poly1305 under the
//! device-bound storage key (Keychain on iOS, DPAPI on Windows). Table names and
//! numeric ids stay in the clear; contents never do.

use crate::error::{db_err, EngineError, Result};
use crate::events::{ChatScope, HistoryEntry};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use proto::control::UserInfo;
use proto::{MessageId, UserId};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{de::DeserializeOwned, Serialize};
use std::path::Path;

const KV: TableDefinition<&str, &[u8]> = TableDefinition::new("kv");
/// Key: kind(1) | scope id(8, BE) | sent_ms(8, BE) | msg_id(8, BE). Sorted by time.
const HISTORY: TableDefinition<&[u8], &[u8]> = TableDefinition::new("history");
const DIRECTORY: TableDefinition<u64, &[u8]> = TableDefinition::new("directory");
const FILES: TableDefinition<u64, &[u8]> = TableDefinition::new("files");
const OUTBOX: TableDefinition<u64, &[u8]> = TableDefinition::new("outbox");

pub const DB_FILE_NAME: &str = "app.redb";

pub struct Store {
    db: Database,
    cipher: XChaCha20Poly1305,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

fn history_key(scope: ChatScope, sent_ms: u64, msg_id: MessageId) -> [u8; 25] {
    let (kind, id) = match scope {
        ChatScope::Dm { user_id } => (0u8, user_id),
        ChatScope::Room { room_id } => (1u8, room_id),
    };
    let mut key = [0u8; 25];
    key[0] = kind;
    key[1..9].copy_from_slice(&id.to_be_bytes());
    key[9..17].copy_from_slice(&sent_ms.to_be_bytes());
    key[17..25].copy_from_slice(&msg_id.to_be_bytes());
    key
}

fn scope_bounds(scope: ChatScope) -> ([u8; 25], [u8; 25]) {
    (
        history_key(scope, 0, 0),
        history_key(scope, u64::MAX, u64::MAX),
    )
}

impl Store {
    pub fn open(dir: &Path, key: &[u8; 32]) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let db = Database::create(dir.join(DB_FILE_NAME)).map_err(db_err)?;
        let cipher = XChaCha20Poly1305::new_from_slice(key)
            .map_err(|e| EngineError::Crypto(format!("storage key: {e}")))?;
        let store = Self { db, cipher };
        // Create every table up front so read transactions never hit a missing table.
        let txn = store.db.begin_write().map_err(db_err)?;
        {
            txn.open_table(KV).map_err(db_err)?;
            txn.open_table(HISTORY).map_err(db_err)?;
            txn.open_table(DIRECTORY).map_err(db_err)?;
            txn.open_table(FILES).map_err(db_err)?;
            txn.open_table(OUTBOX).map_err(db_err)?;
        }
        txn.commit().map_err(db_err)?;
        Ok(store)
    }

    /// nonce(24) || ciphertext. `aad` binds the value to its key so rows cannot be
    /// swapped around inside the file.
    fn seal(&self, plain: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        let nonce_bytes: [u8; 24] = crate::util::random_bytes();
        let nonce = XNonce::from(nonce_bytes);
        let ct = self
            .cipher
            .encrypt(&nonce, Payload { msg: plain, aad })
            .map_err(|_| EngineError::Crypto("seal failed".into()))?;
        let mut out = nonce_bytes.to_vec();
        out.extend(ct);
        Ok(out)
    }

    fn unseal(&self, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() < 24 {
            return Err(EngineError::Crypto("sealed value too short".into()));
        }
        let nonce_bytes: [u8; 24] = sealed[..24]
            .try_into()
            .map_err(|_| EngineError::Crypto("nonce".into()))?;
        let nonce = XNonce::from(nonce_bytes);
        self.cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &sealed[24..],
                    aad,
                },
            )
            .map_err(|_| {
                EngineError::Crypto("unseal failed (wrong storage key or corrupt data)".into())
            })
    }

    fn seal_value<T: Serialize>(&self, value: &T, aad: &[u8]) -> Result<Vec<u8>> {
        self.seal(&proto::encode(value)?, aad)
    }

    fn unseal_value<T: DeserializeOwned>(&self, sealed: &[u8], aad: &[u8]) -> Result<T> {
        Ok(proto::decode(&self.unseal(sealed, aad)?)?)
    }

    // --- key/value -------------------------------------------------------------

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let table = txn.open_table(KV).map_err(db_err)?;
        match table.get(key).map_err(db_err)? {
            Some(guard) => Ok(Some(self.unseal_value(guard.value(), key.as_bytes())?)),
            None => Ok(None),
        }
    }

    pub fn put<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let sealed = self.seal_value(value, key.as_bytes())?;
        let txn = self.db.begin_write().map_err(db_err)?;
        {
            let mut table = txn.open_table(KV).map_err(db_err)?;
            table.insert(key, sealed.as_slice()).map_err(db_err)?;
        }
        txn.commit().map_err(db_err)
    }

    pub fn delete(&self, key: &str) -> Result<()> {
        let txn = self.db.begin_write().map_err(db_err)?;
        {
            let mut table = txn.open_table(KV).map_err(db_err)?;
            table.remove(key).map_err(db_err)?;
        }
        txn.commit().map_err(db_err)
    }

    // --- chat history ------------------------------------------------------------

    pub fn history_put(&self, entry: &HistoryEntry) -> Result<()> {
        let key = history_key(entry.scope, entry.sent_ms, entry.msg_id);
        let sealed = self.seal_value(entry, &key)?;
        let txn = self.db.begin_write().map_err(db_err)?;
        {
            let mut table = txn.open_table(HISTORY).map_err(db_err)?;
            table
                .insert(key.as_slice(), sealed.as_slice())
                .map_err(db_err)?;
        }
        txn.commit().map_err(db_err)
    }

    /// Newest `limit` entries of a conversation, oldest first.
    pub fn history_list(&self, scope: ChatScope, limit: usize) -> Result<Vec<HistoryEntry>> {
        let (lo, hi) = scope_bounds(scope);
        let txn = self.db.begin_read().map_err(db_err)?;
        let table = txn.open_table(HISTORY).map_err(db_err)?;
        let mut out = Vec::new();
        for item in table
            .range(lo.as_slice()..=hi.as_slice())
            .map_err(db_err)?
            .rev()
        {
            let (k, v) = item.map_err(db_err)?;
            out.push(self.unseal_value::<HistoryEntry>(v.value(), k.value())?);
            if out.len() >= limit {
                break;
            }
        }
        out.reverse();
        Ok(out)
    }

    pub fn history_clear(&self, scope: ChatScope) -> Result<()> {
        let (lo, hi) = scope_bounds(scope);
        let txn = self.db.begin_write().map_err(db_err)?;
        {
            let mut table = txn.open_table(HISTORY).map_err(db_err)?;
            table
                .retain_in(lo.as_slice()..=hi.as_slice(), |_, _| false)
                .map_err(db_err)?;
        }
        txn.commit().map_err(db_err)
    }

    /// Flip the delivered flag of one outgoing entry, if it exists.
    pub fn history_mark_delivered(&self, scope: ChatScope, msg_id: MessageId) -> Result<bool> {
        let (lo, hi) = scope_bounds(scope);
        let txn = self.db.begin_write().map_err(db_err)?;
        let mut updated = false;
        {
            let mut table = txn.open_table(HISTORY).map_err(db_err)?;
            let mut found: Option<(Vec<u8>, HistoryEntry)> = None;
            for item in table.range(lo.as_slice()..=hi.as_slice()).map_err(db_err)? {
                let (k, v) = item.map_err(db_err)?;
                if k.value()[17..25] == msg_id.to_be_bytes() {
                    let entry: HistoryEntry = self.unseal_value(v.value(), k.value())?;
                    found = Some((k.value().to_vec(), entry));
                    break;
                }
            }
            if let Some((key, mut entry)) = found {
                if !entry.delivered {
                    entry.delivered = true;
                    let sealed = self.seal_value(&entry, &key)?;
                    table
                        .insert(key.as_slice(), sealed.as_slice())
                        .map_err(db_err)?;
                    updated = true;
                }
            }
        }
        txn.commit().map_err(db_err)?;
        Ok(updated)
    }

    // --- directory cache ---------------------------------------------------------

    pub fn directory_put_all(&self, users: &[UserInfo]) -> Result<()> {
        let txn = self.db.begin_write().map_err(db_err)?;
        {
            let mut table = txn.open_table(DIRECTORY).map_err(db_err)?;
            table.retain(|_, _| false).map_err(db_err)?;
            for user in users {
                let id = user.account.user_id;
                let sealed = self.seal_value(user, &id.to_be_bytes())?;
                table.insert(id, sealed.as_slice()).map_err(db_err)?;
            }
        }
        txn.commit().map_err(db_err)
    }

    pub fn directory_put(&self, user: &UserInfo) -> Result<()> {
        let id = user.account.user_id;
        let sealed = self.seal_value(user, &id.to_be_bytes())?;
        let txn = self.db.begin_write().map_err(db_err)?;
        {
            let mut table = txn.open_table(DIRECTORY).map_err(db_err)?;
            table.insert(id, sealed.as_slice()).map_err(db_err)?;
        }
        txn.commit().map_err(db_err)
    }

    pub fn directory_get(&self, user_id: UserId) -> Result<Option<UserInfo>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let table = txn.open_table(DIRECTORY).map_err(db_err)?;
        match table.get(user_id).map_err(db_err)? {
            Some(guard) => Ok(Some(
                self.unseal_value(guard.value(), &user_id.to_be_bytes())?,
            )),
            None => Ok(None),
        }
    }

    pub fn directory_all(&self) -> Result<Vec<UserInfo>> {
        self.table_all(DIRECTORY)
            .map(|rows| rows.into_iter().map(|(_, u)| u).collect())
    }

    // --- id-keyed tables: directory, file resume state, offline outbox ----------

    fn table_put<T: Serialize>(
        &self,
        def: TableDefinition<u64, &[u8]>,
        id: u64,
        value: &T,
    ) -> Result<()> {
        let sealed = self.seal_value(value, &id.to_be_bytes())?;
        let txn = self.db.begin_write().map_err(db_err)?;
        {
            let mut table = txn.open_table(def).map_err(db_err)?;
            table.insert(id, sealed.as_slice()).map_err(db_err)?;
        }
        txn.commit().map_err(db_err)
    }

    fn table_delete(&self, def: TableDefinition<u64, &[u8]>, id: u64) -> Result<()> {
        let txn = self.db.begin_write().map_err(db_err)?;
        {
            let mut table = txn.open_table(def).map_err(db_err)?;
            table.remove(id).map_err(db_err)?;
        }
        txn.commit().map_err(db_err)
    }

    fn table_all<T: DeserializeOwned>(
        &self,
        def: TableDefinition<u64, &[u8]>,
    ) -> Result<Vec<(u64, T)>> {
        let txn = self.db.begin_read().map_err(db_err)?;
        let table = txn.open_table(def).map_err(db_err)?;
        let mut out = Vec::new();
        for item in table.range::<u64>(..).map_err(db_err)? {
            let (k, v) = item.map_err(db_err)?;
            let id = k.value();
            out.push((id, self.unseal_value(v.value(), &id.to_be_bytes())?));
        }
        Ok(out)
    }

    pub fn files_put<T: Serialize>(&self, id: u64, value: &T) -> Result<()> {
        self.table_put(FILES, id, value)
    }

    pub fn files_delete(&self, id: u64) -> Result<()> {
        self.table_delete(FILES, id)
    }

    pub fn files_all<T: DeserializeOwned>(&self) -> Result<Vec<(u64, T)>> {
        self.table_all(FILES)
    }

    pub fn outbox_put<T: Serialize>(&self, id: u64, value: &T) -> Result<()> {
        self.table_put(OUTBOX, id, value)
    }

    pub fn outbox_delete(&self, id: u64) -> Result<()> {
        self.table_delete(OUTBOX, id)
    }

    pub fn outbox_all<T: DeserializeOwned>(&self) -> Result<Vec<(u64, T)>> {
        self.table_all(OUTBOX)
    }
}

impl Store {
    /// Exact lookup, used to drop duplicates delivered both live and via the server.
    pub fn history_get(
        &self,
        scope: ChatScope,
        sent_ms: u64,
        msg_id: MessageId,
    ) -> Result<Option<HistoryEntry>> {
        let key = history_key(scope, sent_ms, msg_id);
        let txn = self.db.begin_read().map_err(db_err)?;
        let table = txn.open_table(HISTORY).map_err(db_err)?;
        match table.get(key.as_slice()).map_err(db_err)? {
            Some(guard) => Ok(Some(self.unseal_value(guard.value(), &key)?)),
            None => Ok(None),
        }
    }
}

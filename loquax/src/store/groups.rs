//! The groups a home hosts, in the store (1307,
//! `docs/drafts/group-chats-study-2026-11.md` 5.4).
//!
//! | table | columns | why |
//! |---|---|---|
//! | `groups` | address, name, admin, epoch, created_at | the group; its key is a file ([`crate::home::group`]) |
//! | `group_members` | grp, address, name, flags, added_at, left_at | membership, the epoch's content, the stock/Loquax split |
//! | `group_fanout` | grp, original, member, copy, state, sent_at, proven_at, read_at, digest, propagated, device_at | copy id to original (replies, receipts), the ticks; the digest a held copy went out in (schema 12); a node took it (schema 13); its member's device said it has it (schema 16) |
//!
//! Every row is the owner's (`identity`), as every other table's. The epoch
//! counts membership changes: each add, leave or removal moves it by one,
//! in the same transaction as the change.

use rusqlite::{params, OptionalExtension};

use super::{failed, Store, StoreError};

/// A member's flag: it has shown capability bit 1, it understands groups,
/// and gets its copies with the `lev` group key.
pub const MEMBER_GROUPS: i64 = 1;

/// A member's flag: its proofs come slowly (the study's 5.3, slice G3).
pub const MEMBER_SLOW: i64 = 2;

/// A member's flag: it is the group's admin.
pub const MEMBER_ADMIN: i64 = 4;

/// A member's flag: it joined, by the invitation's Join or by writing to
/// the group.
pub const MEMBER_JOINED: i64 = 8;

/// A member's flag (1307 pass four): its newest message to the group said
/// capability bit 0, it sends read receipts; the writer's "read by N of M"
/// counts only these.
pub const MEMBER_RECEIPTS: i64 = 16;

/// A member's flag (1466): its newest message to the group said capability
/// bit 2, its device says when a copy is on it (`lev` `0x07`).
pub const MEMBER_DEVICES: i64 = 32;

/// A copy's state: handed to the outbox, no proof yet.
pub const COPY_SENT: i64 = 0;

/// A copy's state: its member proved it.
pub const COPY_PROVEN: i64 = 1;

/// A copy's state: the outbox gave up on it.
pub const COPY_ENDED: i64 = 2;

/// A copy's state (1307 G2): held for a slow member's next digest, not
/// sent. Its `copy` is the id the copy would have had alone, so a reply
/// still maps; `sent_at` is the acceptance time until the digest goes.
pub const COPY_HELD: i64 = 3;

/// A group as the store keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// Its LXMF delivery address.
    pub address: [u8; 16],
    pub name: String,
    /// The admin's LXMF address.
    pub admin: [u8; 16],
    pub epoch: u64,
    pub created_at: i64,
}

/// One member of a group, present or gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMember {
    pub address: [u8; 16],
    /// The name the admin gave it, the same for every member.
    pub name: String,
    /// [`MEMBER_GROUPS`], [`MEMBER_SLOW`], [`MEMBER_ADMIN`], [`MEMBER_JOINED`].
    pub flags: i64,
    pub added_at: i64,
    pub left_at: Option<i64>,
}

impl GroupMember {
    /// A member added at `at` with no flags.
    pub fn new(address: [u8; 16], name: &str, at: i64) -> Self {
        Self {
            address,
            name: name.to_string(),
            flags: 0,
            added_at: at,
            left_at: None,
        }
    }

    /// Whether it is in the group now.
    pub fn present(&self) -> bool {
        self.left_at.is_none()
    }

    /// Whether it understands groups.
    pub fn understands_groups(&self) -> bool {
        self.flags & MEMBER_GROUPS != 0
    }
}

/// One copy of a member's message to another member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copy {
    pub group: [u8; 16],
    /// The LXMF id of the member's message as it reached the group.
    pub original: [u8; 32],
    pub member: [u8; 16],
    /// The LXMF id of the copy the home sent.
    pub copy: [u8; 32],
    pub state: i64,
    pub sent_at: i64,
    pub proven_at: Option<i64>,
    pub read_at: Option<i64>,
    /// The LXMF id of the digest this copy went out in, for a held one.
    pub digest: Option<[u8; 32]>,
    /// A propagation node took it (1307 pass four): a restart tries it
    /// direct again, and never uploads it twice.
    pub propagated: bool,
    /// When its member's device said it has it (1466): its `0x07`'s
    /// clock, or its read receipt's.
    pub device_at: Option<i64>,
}

/// What a group's copies of one original say (study 2.3): the `lev` key
/// `0x03` its writer gets, counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Copies proven.
    pub delivered: u64,
    /// Copies read, by a member that sends receipts.
    pub read: u64,
    /// Copies of members present now, or proven before they left.
    pub members: u64,
    /// Of those, the members that send receipts.
    pub readers: u64,
    /// Copies sent alone (not held, in no digest) and not proven.
    pub waiting_alone: u64,
    /// Copies whose member's device said it has them (1466).
    pub on_device: u64,
    /// Of the members counted, those whose device says so (1466,
    /// [`MEMBER_DEVICES`]).
    pub devices: u64,
}

impl Copy {
    /// A copy handed to the outbox at `at`.
    pub fn sent(
        group: [u8; 16],
        original: [u8; 32],
        member: [u8; 16],
        copy: [u8; 32],
        at: i64,
    ) -> Self {
        Self {
            group,
            original,
            member,
            copy,
            state: COPY_SENT,
            sent_at: at,
            proven_at: None,
            read_at: None,
            digest: None,
            propagated: false,
            device_at: None,
        }
    }

    /// A copy held for a slow member's digest, accepted at `at`.
    pub fn held(
        group: [u8; 16],
        original: [u8; 32],
        member: [u8; 16],
        copy: [u8; 32],
        at: i64,
    ) -> Self {
        Self {
            state: COPY_HELD,
            ..Self::sent(group, original, member, copy, at)
        }
    }
}

impl Store {
    /// Keep a new group and its first members, the admin among them, at
    /// epoch 1, in one transaction. A group already kept is an error.
    pub fn group_create(
        &mut self,
        group: &Group,
        members: &[GroupMember],
    ) -> Result<(), StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        tx.execute(
            "INSERT INTO groups(identity, address, name, admin, epoch, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &identity[..],
                &group.address[..],
                group.name,
                &group.admin[..],
                epoch_sql(group.epoch),
                group.created_at
            ],
        )
        .map_err(|e| failed(&path, e))?;
        for member in members {
            put_member(&tx, &identity, &group.address, member).map_err(|e| failed(&path, e))?;
        }
        tx.commit().map_err(|e| failed(&path, e))
    }

    /// Every group this home hosts, the oldest first.
    pub fn groups(&self) -> Result<Vec<Group>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT address, name, admin, epoch, created_at FROM groups \
                 WHERE identity = ?1 ORDER BY created_at, address",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..]], group_row)
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        rows.into_iter().map(|row| self.group_of(row)).collect()
    }

    /// The group at `address`, when this home hosts it.
    pub fn group(&self, address: &[u8; 16]) -> Result<Option<Group>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT address, name, admin, epoch, created_at FROM groups \
                 WHERE identity = ?1 AND address = ?2",
                params![&self.identity[..], &address[..]],
                group_row,
            )
            .optional()
            .map_err(|e| self.failed(e))?;
        row.map(|row| self.group_of(row)).transpose()
    }

    /// Every member a group ever had, present or gone, in the order they
    /// were added (the admin first, then the others as the admin picked
    /// them).
    pub fn group_members(&self, group: &[u8; 16]) -> Result<Vec<GroupMember>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT address, name, flags, added_at, left_at FROM group_members \
                 WHERE identity = ?1 AND grp = ?2 ORDER BY added_at, rowid",
            )
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..], &group[..]], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        rows.into_iter()
            .map(|(address, name, flags, added_at, left_at)| {
                Ok(GroupMember {
                    address: self.address(&address)?,
                    name,
                    flags,
                    added_at,
                    left_at,
                })
            })
            .collect()
    }

    /// Add `member` to `group`, or bring a member that left back with its
    /// new name and flags; the epoch moves by one. Returns the new epoch.
    pub fn group_add(&mut self, group: &[u8; 16], member: &GroupMember) -> Result<u64, StoreError> {
        self.membership_change(group, |tx, identity| {
            put_member(tx, identity, group, member)
        })
    }

    /// `address` left `group` at `at`, or was removed; the epoch moves by
    /// one. A member not present changes nothing and keeps the epoch.
    pub fn group_leave(
        &mut self,
        group: &[u8; 16],
        address: &[u8; 16],
        at: i64,
    ) -> Result<u64, StoreError> {
        let present = self
            .group_members(group)?
            .iter()
            .any(|m| m.address == *address && m.present());
        if !present {
            return Ok(self.group(group)?.map_or(0, |g| g.epoch));
        }
        self.membership_change(group, |tx, identity| {
            tx.execute(
                "UPDATE group_members SET left_at = ?4 \
                 WHERE identity = ?1 AND grp = ?2 AND address = ?3",
                params![&identity[..], &group[..], &address[..], at],
            )
            .map(|_| ())
        })
    }

    /// Set `flags` on a member, beside the ones it has: it showed bit 1, it
    /// joined. The epoch stays: what the members list says did not change.
    pub fn group_flag(
        &mut self,
        group: &[u8; 16],
        address: &[u8; 16],
        flags: i64,
    ) -> Result<(), StoreError> {
        self.conn
            .execute(
                "UPDATE group_members SET flags = flags | ?4 \
                 WHERE identity = ?1 AND grp = ?2 AND address = ?3",
                params![&self.identity[..], &group[..], &address[..], flags],
            )
            .map(|_| ())
            .map_err(|e| self.failed(e))
    }

    /// Keep the copies of one original, all or none. A copy kept before
    /// (the same group, original and member) is left as it was: a fan-out
    /// run again after a restart writes the same rows.
    pub fn group_fanout_add(&mut self, copies: &[Copy]) -> Result<(), StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        for copy in copies {
            tx.execute(
                "INSERT OR IGNORE INTO group_fanout(identity, grp, original, member, copy, \
                 state, sent_at, proven_at, read_at, digest, propagated, device_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    &identity[..],
                    &copy.group[..],
                    &copy.original[..],
                    &copy.member[..],
                    &copy.copy[..],
                    copy.state,
                    copy.sent_at,
                    copy.proven_at,
                    copy.read_at,
                    copy.digest.as_ref().map(|d| &d[..]),
                    copy.propagated,
                    copy.device_at
                ],
            )
            .map_err(|e| failed(&path, e))?;
        }
        tx.commit().map_err(|e| failed(&path, e))
    }

    /// The copy with LXMF id `copy`, when it is one of the fan-out's.
    pub fn group_copy(&self, copy: &[u8; 32]) -> Result<Option<Copy>, StoreError> {
        self.copy_where("copy = ?2", &copy[..])
    }

    /// The copy of `original` the fan-out sent `member`.
    pub fn group_copy_for(
        &self,
        group: &[u8; 16],
        original: &[u8; 32],
        member: &[u8; 16],
    ) -> Result<Option<Copy>, StoreError> {
        let row = self
            .conn
            .query_row(
                &format!("{COPY_SELECT} WHERE identity = ?1 AND grp = ?2 AND original = ?3 AND member = ?4"),
                params![&self.identity[..], &group[..], &original[..], &member[..]],
                copy_row,
            )
            .optional()
            .map_err(|e| self.failed(e))?;
        row.map(|row| self.copy_of(row)).transpose()
    }

    /// Every copy of `original`, in the order of their members.
    pub fn group_copies(
        &self,
        group: &[u8; 16],
        original: &[u8; 32],
    ) -> Result<Vec<Copy>, StoreError> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "{COPY_SELECT} WHERE identity = ?1 AND grp = ?2 AND original = ?3 ORDER BY member"
            ))
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(
                params![&self.identity[..], &group[..], &original[..]],
                copy_row,
            )
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        rows.into_iter().map(|row| self.copy_of(row)).collect()
    }

    /// The copy `copy` was proven at `at`: the member has it. Returns the
    /// copy as it now is, `None` for an id that is no copy. A copy proven
    /// before keeps its first time.
    pub fn group_copy_proven(
        &mut self,
        copy: &[u8; 32],
        at: i64,
    ) -> Result<Option<Copy>, StoreError> {
        self.conn
            .execute(
                "UPDATE group_fanout SET state = ?3, proven_at = COALESCE(proven_at, ?4) \
                 WHERE identity = ?1 AND copy = ?2",
                params![&self.identity[..], &copy[..], COPY_PROVEN, at],
            )
            .map_err(|e| self.failed(e))?;
        self.group_copy(copy)
    }

    /// A propagation node took `copy` (1307 pass four): kept, so a restart
    /// hands it to the outbox as propagated and it is not uploaded again.
    /// `false` for an id that is no copy.
    pub fn group_copy_propagated(&mut self, copy: &[u8; 32]) -> Result<bool, StoreError> {
        self.conn
            .execute(
                "UPDATE group_fanout SET propagated = 1 WHERE identity = ?1 AND copy = ?2",
                params![&self.identity[..], &copy[..]],
            )
            .map(|changed| changed > 0)
            .map_err(|e| self.failed(e))
    }

    /// `member` read its copy `copy` at `at` (1307 pass four): a read
    /// receipt it sent the group named it. The copy as it now is, when this
    /// receipt was the first for it; `None` for an id that is no copy of
    /// `member`'s, or one read before.
    pub fn group_copy_read(
        &mut self,
        copy: &[u8; 32],
        member: &[u8; 16],
        at: i64,
    ) -> Result<Option<Copy>, StoreError> {
        let changed = self
            .conn
            .execute(
                "UPDATE group_fanout SET read_at = ?4, device_at = COALESCE(device_at, ?4) \
                 WHERE identity = ?1 AND copy = ?2 AND member = ?3 AND read_at IS NULL",
                params![&self.identity[..], &copy[..], &member[..], at],
            )
            .map_err(|e| self.failed(e))?;
        match changed {
            0 => Ok(None),
            _ => self.group_copy(copy),
        }
    }

    /// `member`'s device said at `at` it has its copy `copy` (1466): its
    /// `0x07` named it. The copy as it now is, when this was the first word
    /// of its device (a read receipt is one too); `None` for an id that is
    /// no copy of `member`'s, or one said before.
    pub fn group_copy_on_device(
        &mut self,
        copy: &[u8; 32],
        member: &[u8; 16],
        at: i64,
    ) -> Result<Option<Copy>, StoreError> {
        let changed = self
            .conn
            .execute(
                "UPDATE group_fanout SET device_at = ?4 \
                 WHERE identity = ?1 AND copy = ?2 AND member = ?3 AND device_at IS NULL",
                params![&self.identity[..], &copy[..], &member[..], at],
            )
            .map_err(|e| self.failed(e))?;
        match changed {
            0 => Ok(None),
            _ => self.group_copy(copy),
        }
    }

    /// The counts of `original`'s copies in `group` (study 2.3): what its
    /// writer's `0x03` says. A member that left before proving its copy is
    /// not counted: the two ticks wait for the members present.
    pub fn group_counts(
        &self,
        group: &[u8; 16],
        original: &[u8; 32],
    ) -> Result<Counts, StoreError> {
        let members = self.group_members(group)?;
        let mut counts = Counts::default();
        for copy in self.group_copies(group, original)? {
            let Some(member) = members.iter().find(|m| m.address == copy.member) else {
                continue;
            };
            let proven = copy.state == COPY_PROVEN;
            if !(member.present() || proven) {
                continue;
            }
            counts.members += 1;
            counts.delivered += u64::from(proven);
            let reader = member.flags & MEMBER_RECEIPTS != 0;
            counts.readers += u64::from(reader);
            counts.read += u64::from(reader && copy.read_at.is_some());
            counts.waiting_alone += u64::from(copy.state == COPY_SENT && copy.digest.is_none());
            counts.on_device += u64::from(copy.device_at.is_some());
            counts.devices += u64::from(member.flags & MEMBER_DEVICES != 0);
        }
        Ok(counts)
    }

    /// The group at `group` is gone (1307 pass four, its admin deleted
    /// it): its row, its members, its copies and the device's own row of
    /// it, in one transaction. Deleting a group not kept changes nothing.
    pub fn group_delete(&mut self, group: &[u8; 16]) -> Result<(), StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        for sql in [
            "DELETE FROM group_fanout WHERE identity = ?1 AND grp = ?2",
            "DELETE FROM group_members WHERE identity = ?1 AND grp = ?2",
            "DELETE FROM groups WHERE identity = ?1 AND address = ?2",
            "DELETE FROM client_groups WHERE identity = ?1 AND address = ?2",
        ] {
            tx.execute(sql, params![&identity[..], &group[..]])
                .map_err(|e| failed(&path, e))?;
        }
        tx.commit().map_err(|e| failed(&path, e))
    }

    /// The outbox gave up on `copy`; a proven copy stays proven.
    pub fn group_copy_ended(&mut self, copy: &[u8; 32]) -> Result<(), StoreError> {
        self.conn
            .execute(
                "UPDATE group_fanout SET state = ?3 \
                 WHERE identity = ?1 AND copy = ?2 AND state = ?4",
                params![&self.identity[..], &copy[..], COPY_ENDED, COPY_SENT],
            )
            .map(|_| ())
            .map_err(|e| self.failed(e))
    }

    /// Every copy of `group` sent alone and not proven yet, the oldest
    /// first (1307 G2): what a restart hands the outbox again, rebuilt
    /// from the archived original.
    pub fn group_copies_open(&self, group: &[u8; 16]) -> Result<Vec<Copy>, StoreError> {
        self.copies_select(
            "grp = ?2 AND state = 0 AND digest IS NULL ORDER BY sent_at, rowid",
            group,
        )
    }

    /// How many copies of every group this home hosts are sent alone and
    /// not proven: what the restart's [`Store::group_copies_open`] hands
    /// the groups' outboxes, counted for the `HOME up` line (1406: it
    /// counted the home's own outbox alone, 9 where 27 went out).
    pub fn group_copies_open_count(&self) -> Result<i64, StoreError> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM group_fanout \
                 WHERE identity = ?1 AND state = 0 AND digest IS NULL",
                params![&self.identity[..]],
                |row| row.get(0),
            )
            .map_err(|e| self.failed(e))
    }

    /// The row of `copy` names `to` from now on: the copy made again under
    /// a rule that changed its bytes, so its id (1406). Its proof and
    /// everything else find the row under the new id.
    pub fn group_copy_renamed(&mut self, copy: &[u8; 32], to: &[u8; 32]) -> Result<(), StoreError> {
        self.conn
            .execute(
                "UPDATE group_fanout SET copy = ?3 WHERE identity = ?1 AND copy = ?2",
                params![&self.identity[..], &copy[..], &to[..]],
            )
            .map(|_| ())
            .map_err(|e| self.failed(e))
    }

    /// Every copy of `group` held for a digest, the oldest first: by the
    /// acceptance time, and within one second in the order the copies were
    /// booked, which is the order the lines were accepted in.
    pub fn group_copies_held(&self, group: &[u8; 16]) -> Result<Vec<Copy>, StoreError> {
        self.copies_select("grp = ?2 AND state = 3 ORDER BY sent_at, rowid", group)
    }

    /// At a restart: every copy of `group` that went out in a digest with
    /// no proof yet is held again, its time kept, so the next digest
    /// carries it (the digest itself is not rebuilt: its retries lived in
    /// memory). The count held again.
    pub fn group_digests_reopen(&mut self, group: &[u8; 16]) -> Result<usize, StoreError> {
        self.conn
            .execute(
                "UPDATE group_fanout SET state = ?3, digest = NULL \
                 WHERE identity = ?1 AND grp = ?2 AND state = ?4 AND digest IS NOT NULL",
                params![&self.identity[..], &group[..], COPY_HELD, COPY_SENT],
            )
            .map_err(|e| self.failed(e))
    }

    /// A copy that went out in the digest with LXMF id `digest`.
    pub fn group_digest_copy(&self, digest: &[u8; 32]) -> Result<Option<Copy>, StoreError> {
        self.copy_where("digest = ?2", &digest[..])
    }

    /// Every copy that went out in the digest `digest`, in the order the
    /// digest carries them.
    pub fn group_digest_copies(&self, digest: &[u8; 32]) -> Result<Vec<Copy>, StoreError> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "{COPY_SELECT} WHERE identity = ?1 AND digest = ?2 ORDER BY sent_at, rowid"
            ))
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..], &digest[..]], copy_row)
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        rows.into_iter().map(|row| self.copy_of(row)).collect()
    }

    /// The held copies `copies` went out in the digest `digest` at `at`:
    /// sent, and `sent_at` from now on the digest's time, which the
    /// member's proof is measured from.
    pub fn group_digest_sent(
        &mut self,
        copies: &[[u8; 32]],
        digest: &[u8; 32],
        at: i64,
    ) -> Result<(), StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        for copy in copies {
            tx.execute(
                "UPDATE group_fanout SET state = ?3, digest = ?4, sent_at = ?5 \
                 WHERE identity = ?1 AND copy = ?2 AND state IN (0, 3)",
                params![&identity[..], &copy[..], COPY_SENT, &digest[..], at],
            )
            .map_err(|e| failed(&path, e))?;
        }
        tx.commit().map_err(|e| failed(&path, e))
    }

    /// The digest `digest` was proven at `at`, or (`proven` false) the
    /// outbox gave up on it: every copy in it changes with it. The count
    /// of copies changed; 0 for an id that is no digest (and for an end
    /// after the proof).
    pub fn group_digest_changed(
        &mut self,
        digest: &[u8; 32],
        proven: bool,
        at: i64,
    ) -> Result<usize, StoreError> {
        let changed = if proven {
            self.conn.execute(
                "UPDATE group_fanout SET state = ?3, proven_at = COALESCE(proven_at, ?4) \
                 WHERE identity = ?1 AND digest = ?2",
                params![&self.identity[..], &digest[..], COPY_PROVEN, at],
            )
        } else {
            self.conn.execute(
                "UPDATE group_fanout SET state = ?3 \
                 WHERE identity = ?1 AND digest = ?2 AND state = ?4",
                params![&self.identity[..], &digest[..], COPY_ENDED, COPY_SENT],
            )
        };
        changed.map_err(|e| self.failed(e))
    }

    /// How long `member`'s last `n` proofs in `group` took, seconds, the
    /// newest first: from a copy's sending, or its digest's, to its proof.
    /// A digest counts once (study 5.3, the slow member's measure).
    pub fn group_proof_times(
        &self,
        group: &[u8; 16],
        member: &[u8; 16],
        n: usize,
    ) -> Result<Vec<i64>, StoreError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT MAX(proven_at - sent_at), MAX(proven_at) FROM group_fanout \
                 WHERE identity = ?1 AND grp = ?2 AND member = ?3 AND state = 1 \
                 GROUP BY COALESCE(digest, copy) ORDER BY 2 DESC, 1 DESC LIMIT ?4",
            )
            .map_err(|e| self.failed(e))?;
        statement
            .query_map(
                params![
                    &self.identity[..],
                    &group[..],
                    &member[..],
                    i64::try_from(n).unwrap_or(i64::MAX)
                ],
                |row| row.get::<_, i64>(0),
            )
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))
    }

    /// Clear `flags` of `member` in `group`; the epoch stays.
    pub fn group_unflag(
        &mut self,
        group: &[u8; 16],
        address: &[u8; 16],
        flags: i64,
    ) -> Result<(), StoreError> {
        self.conn
            .execute(
                "UPDATE group_members SET flags = flags & ~?4 \
                 WHERE identity = ?1 AND grp = ?2 AND address = ?3",
                params![&self.identity[..], &group[..], &address[..], flags],
            )
            .map(|_| ())
            .map_err(|e| self.failed(e))
    }

    fn copies_select(&self, condition: &str, group: &[u8; 16]) -> Result<Vec<Copy>, StoreError> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "{COPY_SELECT} WHERE identity = ?1 AND {condition}"
            ))
            .map_err(|e| self.failed(e))?;
        let rows = statement
            .query_map(params![&self.identity[..], &group[..]], copy_row)
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| self.failed(e))?;
        rows.into_iter().map(|row| self.copy_of(row)).collect()
    }

    fn membership_change(
        &mut self,
        group: &[u8; 16],
        change: impl FnOnce(&rusqlite::Transaction<'_>, &[u8; 16]) -> rusqlite::Result<()>,
    ) -> Result<u64, StoreError> {
        let identity = self.identity;
        let path = self.path.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| failed(&path, e))?;
        change(&tx, &identity).map_err(|e| failed(&path, e))?;
        let changed = tx
            .execute(
                "UPDATE groups SET epoch = epoch + 1 WHERE identity = ?1 AND address = ?2",
                params![&identity[..], &group[..]],
            )
            .map_err(|e| failed(&path, e))?;
        if changed == 0 {
            return Err(failed(&path, "no such group"));
        }
        let epoch: i64 = tx
            .query_row(
                "SELECT epoch FROM groups WHERE identity = ?1 AND address = ?2",
                params![&identity[..], &group[..]],
                |row| row.get(0),
            )
            .map_err(|e| failed(&path, e))?;
        tx.commit().map_err(|e| failed(&path, e))?;
        u64::try_from(epoch).map_err(|_| failed(&path, "a negative epoch"))
    }

    fn copy_where(&self, condition: &str, key: &[u8]) -> Result<Option<Copy>, StoreError> {
        let row = self
            .conn
            .query_row(
                &format!("{COPY_SELECT} WHERE identity = ?1 AND {condition}"),
                params![&self.identity[..], key],
                copy_row,
            )
            .optional()
            .map_err(|e| self.failed(e))?;
        row.map(|row| self.copy_of(row)).transpose()
    }

    fn group_of(&self, row: GroupRow) -> Result<Group, StoreError> {
        let (address, name, admin, epoch, created_at) = row;
        Ok(Group {
            address: self.address(&address)?,
            name,
            admin: self.address(&admin)?,
            epoch: u64::try_from(epoch).map_err(|_| self.failed("a negative epoch"))?,
            created_at,
        })
    }

    fn copy_of(&self, row: CopyRow) -> Result<Copy, StoreError> {
        let (
            (group, original, member, copy, state, sent_at, proven_at, read_at, digest, propagated),
            device_at,
        ) = row;
        let id = |bytes: Vec<u8>| {
            <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| StoreError::Corrupt {
                path: self.path.clone(),
                detail: "a message id is not 32 bytes".to_string(),
            })
        };
        Ok(Copy {
            group: self.address(&group)?,
            original: id(original)?,
            member: self.address(&member)?,
            copy: id(copy)?,
            state,
            sent_at,
            proven_at,
            read_at,
            digest: digest.map(id).transpose()?,
            propagated,
            device_at,
        })
    }
}

type GroupRow = (Vec<u8>, String, Vec<u8>, i64, i64);

type CopyRow = (
    (
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        i64,
        i64,
        Option<i64>,
        Option<i64>,
        Option<Vec<u8>>,
        bool,
    ),
    Option<i64>,
);

const COPY_SELECT: &str = "SELECT grp, original, member, copy, state, sent_at, proven_at, \
                           read_at, digest, propagated, device_at FROM group_fanout";

fn group_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GroupRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn copy_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CopyRow> {
    Ok((
        (
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
        ),
        row.get(10)?,
    ))
}

fn put_member(
    tx: &rusqlite::Transaction<'_>,
    identity: &[u8; 16],
    group: &[u8; 16],
    member: &GroupMember,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO group_members(identity, grp, address, name, flags, added_at, left_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(identity, grp, address) DO UPDATE SET name = excluded.name, \
         flags = excluded.flags, added_at = excluded.added_at, left_at = excluded.left_at",
        params![
            &identity[..],
            &group[..],
            &member.address[..],
            member.name,
            member.flags,
            member.added_at,
            member.left_at
        ],
    )
    .map(|_| ())
}

fn epoch_sql(epoch: u64) -> i64 {
    i64::try_from(epoch).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: [u8; 16] = [0x01; 16];
    const GROUP: [u8; 16] = [0x22; 16];
    const LEW: [u8; 16] = [0x11; 16];
    const ANNA: [u8; 16] = [0x55; 16];
    const BOB: [u8; 16] = [0x66; 16];

    fn familie() -> Group {
        Group {
            address: GROUP,
            name: "Familie".into(),
            admin: LEW,
            epoch: 1,
            created_at: 100,
        }
    }

    fn opened() -> (tempfile::TempDir, Store) {
        let home = tempfile::tempdir().expect("home");
        let store = Store::open_in(home.path(), US).expect("open");
        (home, store)
    }

    #[test]
    fn a_group_is_kept_with_its_members_and_comes_back() {
        let (home, mut store) = opened();
        let mut lew = GroupMember::new(LEW, "Lew", 100);
        lew.flags = MEMBER_ADMIN | MEMBER_GROUPS | MEMBER_JOINED;
        store
            .group_create(
                &familie(),
                &[lew.clone(), GroupMember::new(ANNA, "Anna", 100)],
            )
            .expect("create");
        assert!(store.group_create(&familie(), &[]).is_err(), "kept twice");
        drop(store);
        let store = Store::open_in(home.path(), US).expect("again");
        assert_eq!(store.groups().expect("groups"), [familie()]);
        assert_eq!(store.group(&GROUP).expect("group"), Some(familie()));
        assert_eq!(store.group(&BOB).expect("none"), None);
        let members = store.group_members(&GROUP).expect("members");
        assert_eq!(members.len(), 2);
        assert_eq!(members[0], lew);
        assert!(!members[1].understands_groups() && members[1].present());
        let other = Store::open_in(home.path(), [0x02; 16]).expect("other owner");
        assert!(other.groups().expect("groups").is_empty());
    }

    #[test]
    fn every_membership_change_moves_the_epoch_and_flags_do_not() {
        let (_home, mut store) = opened();
        store
            .group_create(&familie(), &[GroupMember::new(LEW, "Lew", 100)])
            .expect("create");
        assert_eq!(
            store
                .group_add(&GROUP, &GroupMember::new(ANNA, "Anna", 110))
                .expect("add"),
            2
        );
        store
            .group_flag(&GROUP, &ANNA, MEMBER_GROUPS)
            .expect("flag");
        store
            .group_flag(&GROUP, &ANNA, MEMBER_JOINED)
            .expect("flag");
        assert_eq!(store.group(&GROUP).expect("g").map(|g| g.epoch), Some(2));
        assert_eq!(store.group_leave(&GROUP, &ANNA, 120).expect("leave"), 3);
        assert_eq!(
            store.group_leave(&GROUP, &ANNA, 130).expect("leave"),
            3,
            "gone already"
        );
        let anna = store.group_members(&GROUP).expect("m").remove(1);
        assert_eq!(anna.left_at, Some(120));
        assert_eq!(anna.flags, MEMBER_GROUPS | MEMBER_JOINED);
        assert_eq!(
            store
                .group_add(&GROUP, &GroupMember::new(ANNA, "Anna", 140))
                .expect("add"),
            4
        );
        assert!(store.group_members(&GROUP).expect("m")[1].present());
        assert!(store
            .group_add(&BOB, &GroupMember::new(ANNA, "Anna", 1))
            .is_err());
    }

    #[test]
    fn copies_map_both_ways_and_a_proof_is_kept_once() {
        let (_home, mut store) = opened();
        store
            .group_create(&familie(), &[GroupMember::new(LEW, "Lew", 100)])
            .expect("create");
        let original = [0xaa; 32];
        let copies = [
            Copy::sent(GROUP, original, ANNA, [0xa1; 32], 200),
            Copy::sent(GROUP, original, BOB, [0xb1; 32], 200),
        ];
        store.group_fanout_add(&copies).expect("add");
        store
            .group_fanout_add(&copies)
            .expect("again changes nothing");
        assert_eq!(store.group_copies(&GROUP, &original).expect("c"), copies);
        assert_eq!(
            store.group_copy_for(&GROUP, &original, &BOB).expect("b"),
            Some(copies[1].clone())
        );
        assert_eq!(
            store.group_copy(&[0xa1; 32]).expect("a"),
            Some(copies[0].clone())
        );
        let proven = store
            .group_copy_proven(&[0xa1; 32], 205)
            .expect("proof")
            .expect("a copy");
        assert_eq!((proven.state, proven.proven_at), (COPY_PROVEN, Some(205)));
        let again = store
            .group_copy_proven(&[0xa1; 32], 300)
            .expect("p")
            .expect("c");
        assert_eq!(again.proven_at, Some(205));
        store.group_copy_ended(&[0xa1; 32]).expect("end");
        store.group_copy_ended(&[0xb1; 32]).expect("end");
        let after = store.group_copies(&GROUP, &original).expect("c");
        assert_eq!((after[0].state, after[1].state), (COPY_PROVEN, COPY_ENDED));
        assert_eq!(store.group_copy_proven(&[0x77; 32], 1).expect("p"), None);
    }

    /// A schema 10 store, as 1273's loquax wrote it, gains the three group
    /// tables and nothing else; a store of schema 11 without one of them
    /// is refused.
    #[test]
    fn a_schema_10_store_migrates_with_no_group() {
        use super::super::*;
        let home = tempfile::tempdir().expect("home");
        let conn = rusqlite::Connection::open(home.path().join(STORE_FILE)).expect("create");
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA} {SCHEMA_V2} {SCHEMA_V3} {SCHEMA_V4} {SCHEMA_V5} {SCHEMA_V6} \
             {SCHEMA_V7} {SCHEMA_V8} {SCHEMA_V9} {SCHEMA_V10} PRAGMA user_version = 10; COMMIT;"
        ))
        .expect("schema 10");
        drop(conn);
        let mut store = Store::open_in(home.path(), US).expect("schema 10 opens");
        assert!(store.groups().expect("groups").is_empty());
        store
            .group_create(&familie(), &[GroupMember::new(LEW, "Lew", 100)])
            .expect("a group in the migrated store");
        drop(store);
        let conn = rusqlite::Connection::open(home.path().join(STORE_FILE)).expect("open");
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        conn.execute_batch("DROP TABLE group_fanout").expect("drop");
        drop(conn);
        let Err(refused) = Store::open_in(home.path(), US) else {
            panic!("a store without group_fanout opened");
        };
        assert!(
            refused
                .to_string()
                .contains("table group_fanout is missing"),
            "{refused}"
        );
    }

    /// 1307 G2: a schema 11 store, as G1's loquax wrote it, gains the
    /// digest column and the device's group table, its copies kept; a
    /// store of schema 12 without `client_groups` is refused.
    #[test]
    fn a_schema_11_store_migrates_and_keeps_its_copies() {
        use super::super::*;
        let home = tempfile::tempdir().expect("home");
        let conn = rusqlite::Connection::open(home.path().join(STORE_FILE)).expect("create");
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA} {SCHEMA_V2} {SCHEMA_V3} {SCHEMA_V4} {SCHEMA_V5} {SCHEMA_V6} \
             {SCHEMA_V7} {SCHEMA_V8} {SCHEMA_V9} {SCHEMA_V10} {SCHEMA_V11} \
             PRAGMA user_version = 11; COMMIT;"
        ))
        .expect("schema 11");
        drop(conn);
        let mut store = Store::open_in(home.path(), US).expect("schema 11 opens");
        store
            .group_create(&familie(), &[GroupMember::new(LEW, "Lew", 100)])
            .expect("group");
        store
            .group_fanout_add(&[Copy::held(GROUP, [1; 32], LEW, [2; 32], 100)])
            .expect("held");
        store
            .group_digest_sent(&[[2; 32]], &[3; 32], 160)
            .expect("digest");
        let copy = store
            .group_digest_copy(&[3; 32])
            .expect("row")
            .expect("copy");
        assert_eq!(
            (copy.copy, copy.digest, copy.sent_at),
            ([2; 32], Some([3; 32]), 160)
        );
        assert_eq!(store.group_digests_reopen(&GROUP).expect("reopen"), 1);
        assert_eq!(store.group_copies_held(&GROUP).expect("held").len(), 1);
        drop(store);
        let conn = rusqlite::Connection::open(home.path().join(STORE_FILE)).expect("open");
        conn.execute_batch("DROP TABLE client_groups")
            .expect("drop");
        drop(conn);
        let Err(refused) = Store::open_in(home.path(), US) else {
            panic!("a store without client_groups opened");
        };
        assert!(
            refused
                .to_string()
                .contains("table client_groups is missing"),
            "{refused}"
        );
    }

    /// 1307 G2: lines accepted in one second are held and resent in the
    /// order they were accepted, not in the order of their hashes (the
    /// first line here has the larger id, so a sort by id would swap them).
    #[test]
    fn copies_of_one_second_keep_their_acceptance_order() {
        let (_home, mut store) = opened();
        store
            .group_create(&familie(), &[GroupMember::new(LEW, "Lew", 100)])
            .expect("group");
        store
            .group_fanout_add(&[Copy::held(GROUP, [9; 32], LEW, [0x19; 32], 100)])
            .expect("first");
        store
            .group_fanout_add(&[Copy::held(GROUP, [1; 32], LEW, [0x11; 32], 100)])
            .expect("second");
        store
            .group_fanout_add(&[Copy::sent(GROUP, [8; 32], LEW, [0x28; 32], 100)])
            .expect("third");
        store
            .group_fanout_add(&[Copy::sent(GROUP, [2; 32], LEW, [0x22; 32], 100)])
            .expect("fourth");
        let held: Vec<[u8; 32]> = store
            .group_copies_held(&GROUP)
            .expect("held")
            .iter()
            .map(|c| c.original)
            .collect();
        assert_eq!(held, [[9; 32], [1; 32]]);
        let open: Vec<[u8; 32]> = store
            .group_copies_open(&GROUP)
            .expect("open")
            .iter()
            .map(|c| c.original)
            .collect();
        assert_eq!(open, [[8; 32], [2; 32]]);
        assert_eq!(store.group_copies_open_count().expect("count"), 2);
        // A proven copy is open no more; a renamed one keeps its place.
        store.group_copy_proven(&[0x28; 32], 200).expect("proven");
        assert_eq!(store.group_copies_open_count().expect("count"), 1);
        store
            .group_copy_renamed(&[0x22; 32], &[0x33; 32])
            .expect("renamed");
        let open = store.group_copies_open(&GROUP).expect("open");
        assert_eq!(
            open.iter()
                .map(|c| (c.original, c.copy))
                .collect::<Vec<_>>(),
            [([2; 32], [0x33; 32])]
        );
        assert!(store.group_copy(&[0x22; 32]).expect("old").is_none());
    }
}

use std::collections::HashSet;
use std::path::Path;

use anyhow::{anyhow, Result};
use rand::Rng;
use rusqlite::{Connection, OptionalExtension};

const SCHEMA: &str = "
create table if not exists checkpoint (
  id          text primary key,
  parent_id   text references checkpoint(id),
  label       text,
  created_at  integer not null,
  image_path  text not null,
  size        integer
);
create table if not exists computer (
  name        text primary key,
  head_id     text references checkpoint(id),
  created_at  integer not null,
  config_json text
);
";

pub struct Store {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct Computer {
    pub name: String,
    pub head_id: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub id: String,
    pub parent_id: Option<String>,
    pub label: Option<String>,
    pub created_at: i64,
    pub image_path: String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn new_checkpoint_id() -> String {
        let mut rng = rand::thread_rng();
        format!("{:012x}", rng.gen::<u64>() & 0xff_ffff_ffff_ffff)
    }

    // --- computers ---

    pub fn get_computer(&self, name: &str) -> Result<Option<Computer>> {
        Ok(self
            .conn
            .query_row(
                "select name, head_id, created_at from computer where name = ?1",
                [name],
                |r| {
                    Ok(Computer {
                        name: r.get(0)?,
                        head_id: r.get(1)?,
                        created_at: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn create_computer(
        &self,
        name: &str,
        head_id: Option<&str>,
        created_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "insert into computer (name, head_id, created_at) values (?1, ?2, ?3)",
            rusqlite::params![name, head_id, created_at],
        )?;
        Ok(())
    }

    pub fn set_head(&self, name: &str, head_id: &str) -> Result<()> {
        let n = self.conn.execute(
            "update computer set head_id = ?2 where name = ?1",
            rusqlite::params![name, head_id],
        )?;
        if n == 0 {
            return Err(anyhow!("no such computer: {name}"));
        }
        Ok(())
    }

    pub fn list_computers(&self) -> Result<Vec<Computer>> {
        let mut stmt = self
            .conn
            .prepare("select name, head_id, created_at from computer order by name")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Computer {
                    name: r.get(0)?,
                    head_id: r.get(1)?,
                    created_at: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn delete_computer(&self, name: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("delete from computer where name = ?1", [name])?
            > 0)
    }

    // --- checkpoints ---

    pub fn add_checkpoint(
        &self,
        id: &str,
        parent_id: Option<&str>,
        label: Option<&str>,
        image_path: &str,
        created_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "insert into checkpoint (id, parent_id, label, created_at, image_path)
             values (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, parent_id, label, created_at, image_path],
        )?;
        Ok(())
    }

    pub fn get_checkpoint(&self, id: &str) -> Result<Option<Checkpoint>> {
        Ok(self
            .conn
            .query_row(
                "select id, parent_id, label, created_at, image_path from checkpoint where id = ?1",
                [id],
                Self::map_checkpoint,
            )
            .optional()?)
    }

    /// Checkpoints from a computer's head back to its root, newest first.
    pub fn history(&self, name: &str) -> Result<Vec<Checkpoint>> {
        let computer = self
            .get_computer(name)?
            .ok_or_else(|| anyhow!("no such computer: {name}"))?;
        let mut out = Vec::new();
        let mut cursor = computer.head_id;
        while let Some(id) = cursor {
            match self.get_checkpoint(&id)? {
                Some(cp) => {
                    cursor = cp.parent_id.clone();
                    out.push(cp);
                }
                None => break,
            }
        }
        Ok(out)
    }

    /// Resolve a ref (`name` for a computer's head, or `name@label` for a labeled
    /// checkpoint in that computer's ancestry) to a checkpoint.
    pub fn resolve_ref(&self, reference: &str) -> Result<Checkpoint> {
        let (name, label) = match reference.split_once('@') {
            Some((n, l)) => (n, Some(l)),
            None => (reference, None),
        };
        match label {
            None => {
                let computer = self
                    .get_computer(name)?
                    .ok_or_else(|| anyhow!("no such computer: {name}"))?;
                let head = computer
                    .head_id
                    .ok_or_else(|| anyhow!("computer '{name}' has no checkpoints yet"))?;
                self.get_checkpoint(&head)?
                    .ok_or_else(|| anyhow!("dangling head for '{name}'"))
            }
            Some(label) => self
                .history(name)?
                .into_iter()
                .find(|cp| cp.label.as_deref() == Some(label))
                .ok_or_else(|| anyhow!("no checkpoint labeled '{label}' in '{name}'")),
        }
    }

    // --- gc ---

    /// Every checkpoint reachable from some computer's head via parent links.
    pub fn reachable_checkpoints(&self) -> Result<HashSet<String>> {
        let mut reachable = HashSet::new();
        for computer in self.list_computers()? {
            let mut cursor = computer.head_id;
            while let Some(id) = cursor {
                if !reachable.insert(id.clone()) {
                    break; // a shared ancestor, already walked
                }
                cursor = self.get_checkpoint(&id)?.and_then(|cp| cp.parent_id);
            }
        }
        Ok(reachable)
    }

    pub fn all_checkpoints(&self) -> Result<Vec<Checkpoint>> {
        let mut stmt = self
            .conn
            .prepare("select id, parent_id, label, created_at, image_path from checkpoint")?;
        let rows = stmt
            .query_map([], Self::map_checkpoint)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Delete checkpoint rows. Foreign keys are dropped for the batch so a
    /// parent can be removed alongside its unreachable children in any order.
    pub fn delete_checkpoints(&self, ids: &[String]) -> Result<()> {
        self.conn.pragma_update(None, "foreign_keys", "OFF")?;
        let result = (|| {
            for id in ids {
                self.conn
                    .execute("delete from checkpoint where id = ?1", [id])?;
            }
            Ok::<_, rusqlite::Error>(())
        })();
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        result?;
        Ok(())
    }

    fn map_checkpoint(r: &rusqlite::Row) -> rusqlite::Result<Checkpoint> {
        Ok(Checkpoint {
            id: r.get(0)?,
            parent_id: r.get(1)?,
            label: r.get(2)?,
            created_at: r.get(3)?,
            image_path: r.get(4)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp(store: &Store, id: &str, parent: Option<&str>, label: Option<&str>) {
        store
            .add_checkpoint(id, parent, label, &format!("/img/{id}.ext4"), 1)
            .unwrap();
    }

    #[test]
    fn computer_crud_and_head() {
        let s = Store::in_memory().unwrap();
        s.create_computer("alice", None, 1).unwrap();
        assert!(s.get_computer("alice").unwrap().is_some());
        cp(&s, "c1", None, None);
        s.set_head("alice", "c1").unwrap();
        assert_eq!(
            s.get_computer("alice").unwrap().unwrap().head_id.unwrap(),
            "c1"
        );
        assert_eq!(s.list_computers().unwrap().len(), 1);
        assert!(s.delete_computer("alice").unwrap());
        assert!(s.get_computer("alice").unwrap().is_none());
    }

    #[test]
    fn history_walks_parent_chain() {
        let s = Store::in_memory().unwrap();
        cp(&s, "c1", None, None);
        cp(&s, "c2", Some("c1"), Some("clean"));
        cp(&s, "c3", Some("c2"), None);
        s.create_computer("alice", Some("c3"), 1).unwrap();
        let ids: Vec<_> = s
            .history("alice")
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, vec!["c3", "c2", "c1"]);
    }

    #[test]
    fn reachable_spans_all_computer_ancestries() {
        let s = Store::in_memory().unwrap();
        cp(&s, "c1", None, None);
        cp(&s, "c2", Some("c1"), None);
        cp(&s, "c3", Some("c2"), None); // alice head
        cp(&s, "c4", Some("c2"), None); // bob head, branch sharing c1/c2
        cp(&s, "c5", Some("c1"), None); // orphan: no computer points here
        s.create_computer("alice", Some("c3"), 1).unwrap();
        s.create_computer("bob", Some("c4"), 1).unwrap();

        let reachable = s.reachable_checkpoints().unwrap();
        assert!(["c1", "c2", "c3", "c4"]
            .iter()
            .all(|id| reachable.contains(*id)));
        assert!(!reachable.contains("c5"));

        let dead: Vec<_> = s
            .all_checkpoints()
            .unwrap()
            .into_iter()
            .filter(|c| !reachable.contains(&c.id))
            .map(|c| c.id)
            .collect();
        assert_eq!(dead, vec!["c5"]);
        s.delete_checkpoints(&dead).unwrap();
        assert!(s.get_checkpoint("c5").unwrap().is_none());
        assert!(s.get_checkpoint("c1").unwrap().is_some());
    }

    #[test]
    fn resolve_head_and_label() {
        let s = Store::in_memory().unwrap();
        cp(&s, "c1", None, None);
        cp(&s, "c2", Some("c1"), Some("clean"));
        s.create_computer("alice", Some("c2"), 1).unwrap();
        assert_eq!(s.resolve_ref("alice").unwrap().id, "c2");
        assert_eq!(s.resolve_ref("alice@clean").unwrap().id, "c2");
        assert!(s.resolve_ref("alice@missing").is_err());
        assert!(s.resolve_ref("ghost").is_err());
    }
}

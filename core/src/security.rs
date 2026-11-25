use crate::{Mosh, Object, organization::AUTHORITY};
use anyhow::{Result, ensure};
use rusqlite::{Transaction, TransactionBehavior, params};
use serde_json::{Value, json};

pub fn allowed(right: &str) -> String {
    debug_assert!(matches!(right, "paint" | "content" | "edit" | "manage"));
    let legacy = match right {
        "paint" | "content" => "1",
        "edit" => "g.permission='edit'",
        _ => "0",
    };
    format!("(?1='local' OR NOT EXISTS(SELECT 1 FROM objects s LEFT JOIN coll_security p ON s.key=p.object
      WHERE (p.restricted=1 OR s.secret=1) AND s.missing=0 AND
      (s.key=o.key OR (s.path IS NOT NULL AND o.path IS NOT NULL
        AND substr(o.path,1,length(rtrim(s.path,?2))+1)=rtrim(s.path,?2)||?2))
      AND NOT EXISTS(SELECT 1 FROM owners w JOIN authority a ON a.person=w.person WHERE w.object=s.key)
      AND NOT EXISTS(SELECT 1 FROM guests g WHERE g.object=s.key AND g.person=?1 AND {legacy})
      AND NOT EXISTS(SELECT 1 FROM guest_rights g WHERE g.object=s.key AND g.person=?1 AND g.right='{right}')))")
}

impl Mosh {
    pub(crate) fn permits(&self, object: &Object, right: &str) -> Result<bool> {
        if self.actor == "local" {
            return Ok(true);
        }
        let sql = format!(
            "{AUTHORITY}, o(key,path) AS (VALUES(?3,?4)) SELECT {} FROM o",
            allowed(right)
        );
        Ok(self.db.prepare_cached(&sql)?.query_row(
            params![
                self.actor,
                std::path::MAIN_SEPARATOR_STR,
                object.key,
                object.path
            ],
            |r| r.get(0),
        )?)
    }

    pub(crate) fn require_contents(&self, object: &Object) -> Result<()> {
        ensure!(
            self.permits(object, "content")?,
            "content access is required"
        );
        Ok(())
    }

    pub(crate) fn can_manage(&self, object: &Object) -> Result<bool> {
        Ok(self.owns(&object.id)? || self.explicit_right(object, "manage")?)
    }

    pub(crate) fn explicit_right(&self, object: &Object, right: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM guest_rights WHERE object=? AND person=? AND right=?)",
            params![object.key, self.actor, right],
            |r| r.get(0),
        )?)
    }

    pub fn coll_security(
        &self,
        reference: &str,
        restricted: Option<bool>,
        person: Option<&str>,
        rights: &[String],
    ) -> Result<Value> {
        let item = self.get(reference)?;
        ensure!(
            item.kind != "file" && self.can_manage(&item)?,
            "Coll management permission required"
        );
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        if let Some(value) = restricted {
            tx.execute("INSERT INTO coll_security VALUES(?,?) ON CONFLICT DO UPDATE SET restricted=excluded.restricted",
                params![item.key,value])?;
        }
        if let Some(person) = person {
            ensure!(
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM people WHERE id=?)",
                    [person],
                    |r| r.get::<_, bool>(0)
                )?,
                "unknown person"
            );
            tx.execute(
                "DELETE FROM guest_rights WHERE object=? AND person=?",
                params![item.key, person],
            )?;
            for right in rights {
                ensure!(
                    matches!(
                        right.as_str(),
                        "discover" | "paint" | "content" | "edit" | "manage"
                    ),
                    "unknown permission"
                );
                tx.execute(
                    "INSERT OR IGNORE INTO guest_rights VALUES(?,?,?)",
                    params![item.key, person, right],
                )?;
            }
        }
        if restricted.is_some() || person.is_some() {
            crate::event(
                &tx,
                Some(&item.id),
                "coll-security",
                Some(&self.actor),
                None,
                Some(&json!({"restricted":restricted,"person":person,"rights":rights})),
                "ok",
            )?;
        }
        let result=tx.query_row("SELECT coalesce((SELECT restricted FROM coll_security WHERE object=?1),0),
            (SELECT json_group_array(json_object('person',person,'right',right)) FROM guest_rights WHERE object=?1)",[item.key],
            |r|Ok(json!({"restricted":r.get::<_,bool>(0)?,"grants":serde_json::from_str::<Value>(&r.get::<_,String>(1)?).unwrap_or_default()})))?;
        tx.commit()?;
        Ok(result)
    }
}

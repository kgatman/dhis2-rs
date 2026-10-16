//! User lookups that replace Hibernate queries on `User`.
//!
//! Table and column names come from the generated mappings (`dhis_schema::hibernate`).

use std::sync::LazyLock;

use dhis_schema::hibernate::entity;
use sqlx::PgPool;

const USER: &str = "org.hisp.dhis.user.User";
const USER_GROUP: &str = "org.hisp.dhis.user.UserGroup";

/// `select uid, group uids from User where username = ?`, built from the mappings: the user's
/// `uid`, and the `uid`s of its `groups` (join table of the `set`).
static IDENTITY_SQL: LazyLock<Option<String>> = LazyLock::new(|| identity_sql("username"));

/// The same, by the user's `uid`.
static GROUPS_BY_UID_SQL: LazyLock<Option<String>> = LazyLock::new(|| identity_sql("uid"));

fn identity_sql(by: &str) -> Option<String> {
    let user = entity(USER)?;
    let group = entity(USER_GROUP)?;
    let col = |e: &dhis_schema::hibernate::Entity, p: &str| {
        e.properties
            .get(p)
            .filter(|m| m.kind == "property")
            .and_then(|m| m.column.clone())
    };
    let groups = user.properties.get("groups")?;
    if groups.element_class.as_deref() != Some(USER_GROUP) {
        return None;
    }
    Some(format!(
        "select u.{uid}, array(select g.{guid} from {join} m join {gtable} g on g.{gid} = m.{elem} \
         where m.{key} = u.{uid_pk}) from {utable} u where u.{username} = $1",
        uid = col(user, "uid")?,
        guid = col(group, "uid")?,
        join = groups.table.as_deref()?,
        gtable = group.table.as_deref()?,
        gid = group.id.as_ref()?.column,
        elem = groups.element_column.as_deref()?,
        key = groups.key_column.as_deref()?,
        uid_pk = user.id.as_ref()?.column,
        utable = user.table.as_deref()?,
        username = col(user, by)?,
    ))
}

/// `UserStore#getUserByUsername(username)` (exact match, as basic authentication uses it):
/// the user's uid and the uids of its groups, sorted as Java sorts them (by UTF-16 code units,
/// here ASCII UIDs). `None` when there is no such user.
pub async fn identity_by_username(
    pool: &PgPool,
    username: &str,
) -> Result<Option<(String, Vec<String>)>, sqlx::Error> {
    let Some(sql) = IDENTITY_SQL.as_deref() else {
        return Err(sqlx::Error::Protocol("user mapping incomplete".into()));
    };
    let row: Option<(String, Vec<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(username)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(uid, mut groups)| {
        groups.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        (uid, groups)
    }))
}

/// `InternalHibernateGenericStoreImpl#fetchCurrentUserGroupInfo`: the uids of the user's groups,
/// read from the database (Java caches them per user and invalidates the cache when groups or
/// memberships change). Empty when there is no such user.
pub async fn group_uids_by_uid(pool: &PgPool, uid: &str) -> Result<Vec<String>, sqlx::Error> {
    let Some(sql) = GROUPS_BY_UID_SQL.as_deref() else {
        return Err(sqlx::Error::Protocol("user mapping incomplete".into()));
    };
    let row: Option<(String, Vec<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(uid)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(_, groups)| groups).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_from_mappings() {
        assert_eq!(
            IDENTITY_SQL.as_deref(),
            Some(
                "select u.uid, array(select g.uid from usergroupmembers m join usergroup g on \
                 g.usergroupid = m.usergroupid where m.userid = u.userinfoid) from userinfo u \
                 where u.username = $1"
            )
        );
    }
}

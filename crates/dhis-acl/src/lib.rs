//! Access control: sharing and authorities.
//!
//! Java counterpart: `org.hisp.dhis.security.acl.DefaultAclService` (dhis-service-acl) and
//! `AccessStringHelper` (dhis-api). Ported: `getAccess` (the `access` block of metadata
//! responses) and `canRead` for objects and types.

use dhis_schema::registered::RegisteredSchema;

/// The current user (`UserDetails`): what access decisions depend on.
#[derive(Debug, Clone, Default)]
pub struct User<'a> {
    pub superuser: bool,
    pub uid: &'a str,
    pub group_ids: &'a [String],
    pub authorities: &'a [String],
}

/// An object's sharing (`Sharing`): owner, public access string, user and group accesses as
/// (id, access string).
#[derive(Debug, Clone, Default)]
pub struct Sharing {
    pub owner: Option<String>,
    pub public: Option<String>,
    pub users: Vec<(String, Option<String>)>,
    pub user_groups: Vec<(String, Option<String>)>,
}

/// `org.hisp.dhis.security.acl.Access`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access {
    pub manage: bool,
    pub write: bool,
    pub read: bool,
    pub update: bool,
    pub delete: bool,
    /// `AccessData` (read, write) for data-shareable types.
    pub data: Option<(bool, bool)>,
}

impl Access {
    /// Jackson's rendering: `{"manage","write","read","update","delete"[,"data":{"write","read"}]}`.
    pub fn to_json(&self) -> String {
        let b = |v: bool| if v { "true" } else { "false" };
        let mut s = format!(
            "{{\"manage\":{},\"write\":{},\"read\":{},\"update\":{},\"delete\":{}",
            b(self.manage),
            b(self.write),
            b(self.read),
            b(self.update),
            b(self.delete)
        );
        if let Some((read, write)) = self.data {
            s.push_str(&format!(
                ",\"data\":{{\"write\":{},\"read\":{}}}",
                b(write),
                b(read)
            ));
        }
        s.push('}');
        s
    }
}

/// `AccessStringHelper.Permission`: (character, position).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Read,
    Write,
    DataRead,
    DataWrite,
}

impl Permission {
    fn at(self) -> (u8, usize) {
        match self {
            Permission::Read => (b'r', 0),
            Permission::Write => (b'w', 1),
            Permission::DataRead => (b'r', 2),
            Permission::DataWrite => (b'w', 3),
        }
    }
}

/// `AccessStringHelper.DEFAULT`.
const DEFAULT: &str = "--------";

/// `AccessStringHelper.isEnabled`: a null access string enables everything; an invalid one
/// nothing.
pub fn is_enabled(access: Option<&str>, permission: Permission) -> bool {
    let Some(a) = access else {
        return true;
    };
    let b = a.as_bytes();
    let valid = b.len() == 8
        && a.ends_with("----")
        && (b[0] == b'-' || b[0] == b'r')
        && (b[1] == b'-' || b[1] == b'w')
        && (b[2] == b'-' || b[2] == b'r')
        && (b[3] == b'-' || b[3] == b'w');
    let (c, i) = permission.at();
    valid && b[i] == c
}

impl User<'_> {
    /// `canAccess`: superusers, no authorities required, or any of them.
    fn can_access(&self, any: &[&str]) -> bool {
        self.superuser
            || any.is_empty()
            || any.iter().any(|a| self.authorities.iter().any(|u| u == a))
    }
}

/// The schema's authorities of one type (`getAuthorityByType`).
fn auth<'s>(schema: &'s RegisteredSchema, kind: &str) -> Vec<&'s str> {
    schema.authorities_by_type(kind)
}

/// `aclService.canRead(user, type)`.
pub fn can_read_type(user: &User, schema: &RegisteredSchema) -> bool {
    user.can_access(&auth(schema, "READ"))
}

fn check_owner(user: &User, sharing: &Sharing) -> bool {
    sharing.owner.as_deref().is_none_or(|o| o == user.uid)
}

fn check_sharing_permission(user: &User, sharing: &Sharing, p: Permission) -> bool {
    if is_enabled(sharing.public.as_deref(), p) {
        return true;
    }
    if !user.group_ids.is_empty() {
        for (id, access) in &sharing.user_groups {
            if is_enabled(access.as_deref(), p) && user.group_ids.iter().any(|g| g == id) {
                return true;
            }
        }
    }
    sharing
        .users
        .iter()
        .any(|(id, access)| is_enabled(access.as_deref(), p) && id == user.uid)
}

fn check_metadata_sharing_permission(user: &User, sharing: &Sharing, p: Permission) -> bool {
    check_owner(user, sharing) || check_sharing_permission(user, sharing, p)
}

fn can_make_public(user: &User, schema: &RegisteredSchema) -> bool {
    schema.shareable && user.can_access(&auth(schema, "CREATE_PUBLIC"))
}

fn can_make_private(user: &User, schema: &RegisteredSchema) -> bool {
    schema.shareable && user.can_access(&auth(schema, "CREATE_PRIVATE"))
}

fn check_sharing_access(user: &User, schema: &RegisteredSchema, sharing: &Sharing) -> bool {
    let public = can_make_public(user, schema);
    if sharing.public.as_deref() == Some(DEFAULT) {
        public || can_make_private(user, schema)
    } else {
        public
    }
}

fn write_common_check(user: &User, schema: &RegisteredSchema, sharing: &Sharing) -> bool {
    !schema.shareable
        || (check_sharing_access(user, schema, sharing)
            && check_metadata_sharing_permission(user, sharing, Permission::Write))
}

/// Authorities of `kind`, else the creation ones (`canWrite`/`canUpdate`/`canDelete`).
fn with_fallback<'s>(schema: &'s RegisteredSchema, kind: &str, create: bool) -> Vec<&'s str> {
    let mut any = auth(schema, kind);
    if any.is_empty() {
        if create {
            any.extend(auth(schema, "CREATE"));
        }
        any.extend(auth(schema, "CREATE_PRIVATE"));
        any.extend(auth(schema, "CREATE_PUBLIC"));
    }
    any
}

/// `canRead(user, object)` (not for `CategoryOptionCombo`, which needs its options).
pub fn can_read(user: &User, schema: &RegisteredSchema, sharing: &Sharing) -> bool {
    if user.superuser {
        return true;
    }
    user.can_access(&auth(schema, "READ"))
        && (!schema.shareable
            || sharing.public.is_none()
            || check_metadata_sharing_permission(user, sharing, Permission::Read))
}

fn can_write(user: &User, schema: &RegisteredSchema, sharing: &Sharing) -> bool {
    if user.superuser {
        return true;
    }
    if user.can_access(&with_fallback(schema, "CREATE", false)) {
        write_common_check(user, schema, sharing)
    } else {
        schema.implicit_private_authority && check_sharing_access(user, schema, sharing)
    }
}

fn can_update(user: &User, schema: &RegisteredSchema, sharing: &Sharing) -> bool {
    if user.superuser {
        return true;
    }
    if user.can_access(&with_fallback(schema, "UPDATE", true)) {
        write_common_check(user, schema, sharing)
    } else {
        schema.implicit_private_authority
            && check_sharing_access(user, schema, sharing)
            && check_metadata_sharing_permission(user, sharing, Permission::Write)
    }
}

fn can_delete(user: &User, schema: &RegisteredSchema, sharing: &Sharing) -> bool {
    if user.superuser {
        return true;
    }
    if user.can_access(&with_fallback(schema, "DELETE", true)) {
        !schema.shareable
            || sharing.public.is_none()
            || (check_sharing_access(user, schema, sharing)
                && check_metadata_sharing_permission(user, sharing, Permission::Write))
    } else {
        schema.implicit_private_authority
            && check_metadata_sharing_permission(user, sharing, Permission::Write)
    }
}

fn can_data(user: &User, schema: &RegisteredSchema, sharing: &Sharing, kind: &str) -> bool {
    if user.superuser {
        return true;
    }
    if !user.can_access(&auth(schema, kind)) || !schema.data_shareable {
        return false;
    }
    if kind == "DATA_READ" {
        check_sharing_permission(user, sharing, Permission::DataRead)
            || check_sharing_permission(user, sharing, Permission::DataWrite)
    } else {
        check_sharing_permission(user, sharing, Permission::DataWrite)
    }
}

/// `aclService.getAccess(object, user)`. `None` for `CategoryOptionCombo` (its access depends
/// on its category options; not ported).
pub fn get_access(user: &User, schema: &RegisteredSchema, sharing: &Sharing) -> Option<Access> {
    if schema.klass == "org.hisp.dhis.category.CategoryOptionCombo" {
        return None;
    }
    let data = schema.data_shareable;
    if user.superuser {
        return Some(Access {
            manage: true,
            write: true,
            read: true,
            update: true,
            delete: true,
            data: data.then_some((true, true)),
        });
    }
    let update = can_update(user, schema, sharing);
    Some(Access {
        manage: update,
        write: can_write(user, schema, sharing),
        read: can_read(user, schema, sharing),
        update,
        delete: can_delete(user, schema, sharing),
        data: data.then(|| {
            (
                can_data(user, schema, sharing, "DATA_READ"),
                can_data(user, schema, sharing, "DATA_CREATE"),
            )
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_strings() {
        assert!(is_enabled(None, Permission::Write));
        assert!(is_enabled(Some("rw------"), Permission::Write));
        assert!(!is_enabled(Some("r-------"), Permission::Write));
        assert!(!is_enabled(Some("rw-----x"), Permission::Read));
        assert!(is_enabled(Some("--rw----"), Permission::DataWrite));
    }

    #[test]
    fn constant_access_like_java() {
        let schema = dhis_schema::registered::by_singular("constant").unwrap();
        let groups = vec!["Crp0Grp0001".to_string()];
        let auths: Vec<String> = vec!["F_DATAVALUE_ADD".into()];
        let user = User {
            superuser: false,
            uid: "Crp0User001",
            group_ids: &groups,
            authorities: &auths,
        };
        // measured: corpusbasic on a constant shared r------- with the user
        let shared = Sharing {
            owner: Some("M5zQapPyTZI".into()),
            public: Some("--------".into()),
            users: vec![("Crp0User001".into(), Some("r-------".into()))],
            user_groups: vec![],
        };
        assert_eq!(
            get_access(&user, schema, &shared).unwrap().to_json(),
            r#"{"manage":false,"write":false,"read":true,"update":false,"delete":false}"#
        );
    }
}

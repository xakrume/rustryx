//! The user entry found by the service account.

use super::transport::LdapEntry;

/// A user as the directory describes them.
#[derive(Debug, Clone)]
pub(crate) struct DirectoryUser {
    /// DN exactly as returned: the identity the user bind is made as.
    pub(crate) dn: String,
    /// Name to use for the principal and for later group refreshes.
    pub(crate) username: String,
    /// The entry, kept for its group attribute.
    pub(crate) entry: LdapEntry,
}

/// Outcome of the user search. Only `Found` leads to a bind.
#[derive(Debug)]
pub(crate) enum UserLookup {
    Found(DirectoryUser),
    NotFound,
    /// More than one entry (or a size limit cut the answer short): the user cannot
    /// be told apart from another entry, so nobody is authenticated.
    Ambiguous,
}

impl DirectoryUser {
    /// Builds the user from a search entry.
    ///
    /// The principal gets the directory's spelling of the name (`alice` for a typed
    /// `ALICE`) so that one person is one account, but only when it is the same name
    /// apart from case and padding. A different value (the user logged in with a
    /// mail address, the name attribute is `uid`) would not match the search filter
    /// when the group refresh searches again, so the typed login is kept then.
    pub(crate) fn from_entry(entry: LdapEntry, login: &str, username_attribute: &str) -> Self {
        let username = entry
            .first_value(username_attribute)
            .filter(|value| same_name(value, login))
            .unwrap_or(login)
            .to_owned();
        Self {
            dn: entry.dn.clone(),
            username,
            entry,
        }
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_with_uid(uid: &str) -> LdapEntry {
        LdapEntry::new("uid=alice,ou=people,dc=x").with_attribute("uid", [uid])
    }

    #[test]
    fn takes_the_directory_spelling_when_only_case_differs() {
        let user = DirectoryUser::from_entry(entry_with_uid("alice"), "ALICE", "uid");
        assert_eq!(user.username, "alice");
        assert_eq!(user.dn, "uid=alice,ou=people,dc=x");
    }

    #[test]
    fn takes_the_directory_spelling_when_only_padding_differs() {
        let user = DirectoryUser::from_entry(entry_with_uid("alice"), "alice ", "uid");
        assert_eq!(user.username, "alice");
    }

    #[test]
    fn keeps_the_typed_login_when_the_attribute_is_a_different_name() {
        let user = DirectoryUser::from_entry(entry_with_uid("alice"), "alice@example.org", "uid");
        assert_eq!(user.username, "alice@example.org");
    }

    #[test]
    fn keeps_the_typed_login_when_the_attribute_is_missing() {
        let entry = LdapEntry::new("uid=alice,dc=x");
        let user = DirectoryUser::from_entry(entry, "alice", "uid");
        assert_eq!(user.username, "alice");
    }
}

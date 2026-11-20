use std::fmt;
use std::str::FromStr;

use crate::{Error, ObjectId};

const HEADER: &str = "cas-fs-tree 1\n";

/// The type of an entry in a [`Tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A regular file with the executable bit set.
    Executable,
    /// A symbolic link; the referenced blob holds the link target.
    Symlink,
    /// A sub-directory; the referenced object is another tree.
    Tree,
}

impl EntryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Executable => "exec",
            Self::Symlink => "link",
            Self::Tree => "tree",
        }
    }
}

impl fmt::Display for EntryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EntryKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "file" => Ok(Self::File),
            "exec" => Ok(Self::Executable),
            "link" => Ok(Self::Symlink),
            "tree" => Ok(Self::Tree),
            _ => Err(()),
        }
    }
}

/// One named entry of a [`Tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// File name (a single path component, UTF-8, no `/` or newline).
    pub name: String,
    /// Entry type.
    pub kind: EntryKind,
    /// Id of the blob or sub-tree.
    pub id: ObjectId,
}

/// A directory listing whose id is the hash of its canonical encoding (a Merkle tree).
///
/// Entries are always kept sorted by name, so equal directories always produce equal ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    entries: Vec<TreeEntry>,
}

impl Tree {
    /// Builds a tree from entries. Entries are sorted; duplicate or invalid names are rejected.
    pub fn new(mut entries: Vec<TreeEntry>) -> Result<Self, Error> {
        for entry in &entries {
            validate_name(&entry.name)?;
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        if let Some(pair) = entries.windows(2).find(|w| w[0].name == w[1].name) {
            return Err(Error::InvalidName(pair[0].name.clone()));
        }
        Ok(Self { entries })
    }

    /// The entries, sorted by name.
    pub fn entries(&self) -> &[TreeEntry] {
        &self.entries
    }

    /// Finds an entry by name.
    pub fn get(&self, name: &str) -> Option<&TreeEntry> {
        self.entries
            .binary_search_by(|e| e.name.as_str().cmp(name))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Returns the canonical byte encoding that is hashed to obtain the tree id.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = String::from(HEADER);
        for e in &self.entries {
            out.push_str(e.kind.as_str());
            out.push(' ');
            out.push_str(&e.id.to_hex());
            out.push(' ');
            out.push_str(&e.name);
            out.push('\n');
        }
        out.into_bytes()
    }

    /// Returns the id of this tree.
    pub fn id(&self) -> ObjectId {
        ObjectId::of(&self.encode())
    }

    /// Parses the canonical encoding.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let text = std::str::from_utf8(bytes).map_err(|_| Error::Corrupt("tree is not UTF-8"))?;
        let body = text
            .strip_prefix(HEADER)
            .ok_or(Error::Corrupt("missing tree header"))?;
        let mut entries = Vec::new();
        for line in body.lines() {
            let mut parts = line.splitn(3, ' ');
            let (Some(kind), Some(hex), Some(name)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(Error::Corrupt("malformed tree entry"));
            };
            let kind = kind
                .parse()
                .map_err(|()| Error::Corrupt("unknown tree entry kind"))?;
            let mut bytes = [0u8; 32];
            hex::decode_to_slice(hex, &mut bytes)
                .map_err(|_| Error::Corrupt("invalid id in tree entry"))?;
            entries.push(TreeEntry {
                name: name.to_owned(),
                kind,
                id: ObjectId::from_bytes(bytes),
            });
        }
        Self::new(entries)
    }
}

fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\n', '\0']) {
        return Err(Error::InvalidName(name.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_is_sorted_and_round_trips() {
        let a = ObjectId::of(b"a");
        let tree = Tree::new(vec![
            TreeEntry {
                name: "z".into(),
                kind: EntryKind::File,
                id: a,
            },
            TreeEntry {
                name: "a b".into(),
                kind: EntryKind::Tree,
                id: a,
            },
        ])
        .unwrap();
        assert_eq!(tree.entries()[0].name, "a b");
        let decoded = Tree::decode(&tree.encode()).unwrap();
        assert_eq!(decoded, tree);
        assert_eq!(decoded.id(), tree.id());
    }

    #[test]
    fn rejects_duplicates_and_bad_names() {
        let a = ObjectId::of(b"a");
        let entry = |name: &str| TreeEntry {
            name: name.into(),
            kind: EntryKind::File,
            id: a,
        };
        assert!(Tree::new(vec![entry("x"), entry("x")]).is_err());
        assert!(Tree::new(vec![entry("a/b")]).is_err());
        assert!(Tree::new(vec![entry("..")]).is_err());
    }
}

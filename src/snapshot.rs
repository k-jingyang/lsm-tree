use crate::{
    value::{SeqNo, UserKey, UserValue},
    AbstractTree, AnyTree, KvPair,
};
use std::ops::RangeBounds;

/// A snapshot captures a read-only point-in-time view of the tree at the time the snapshot was created
///
/// As long as the snapshot is open, old versions of objects will not be evicted as to
/// keep the snapshot consistent. Thus, snapshots should only be kept around for as little as possible.
///
/// Snapshots do not persist across restarts.
#[derive(Clone)]
pub struct Snapshot {
    tree: AnyTree,
    seqno: SeqNo,
}

impl Snapshot {
    /// Creates a snapshot
    pub(crate) fn new(tree: AnyTree, seqno: SeqNo) -> Self {
        log::trace!("Opening snapshot with seqno: {seqno}");
        Self { tree, seqno }
    }

    /*  #[doc(hidden)]
    pub fn get_internal_entry<K: AsRef<[u8]>>(&self, key: K) -> crate::Result<Option<Value>> {
        self.tree.get_internal_entry(key, true, Some(self.seqno))
    } */

    /// Retrieves an item from the snapshot.
    ///
    /// # Examples
    ///
    /// ```
    /// # let folder = tempfile::tempdir()?;
    /// use lsm_tree::{AbstractTree, Config, Tree};
    ///
    /// let tree = Config::new(folder).open()?;
    /// let snapshot = tree.snapshot(0);
    ///
    /// tree.insert("a", "my_value", 0);
    ///
    /// let item = snapshot.get("a")?;
    /// assert_eq!(None, item);
    /// #
    /// # Ok::<(), lsm_tree::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn get<K: AsRef<[u8]>>(&self, key: K) -> crate::Result<Option<UserValue>> {
        self.tree.get_with_seqno(key, self.seqno)
    }

    #[allow(clippy::iter_not_returning_iterator)]
    /// Returns an iterator that scans through the entire snapshot.
    ///
    /// Avoid using this function, or limit it as otherwise it may scan a lot of items.
    ///
    /// # Examples
    ///
    /// ```
    /// # let folder = tempfile::tempdir()?;
    /// use lsm_tree::{AbstractTree, Config, Tree};
    ///
    /// let tree = Config::new(folder).open()?;
    ///
    /// tree.insert("a", "abc", 0);
    /// tree.insert("f", "abc", 1);
    /// let snapshot = tree.snapshot(2);
    ///
    /// tree.insert("g", "abc", 2);
    ///
    /// assert_eq!(2, snapshot.iter().count());
    /// #
    /// # Ok::<(), lsm_tree::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    #[must_use]
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = crate::Result<KvPair>> {
        self.tree.iter_with_seqno(self.seqno, None)
    }

    /// Returns an iterator over a range of items in the snapshot.
    ///
    /// Avoid using full or unbounded ranges as they may scan a lot of items (unless limited).
    ///
    /// # Examples
    ///
    /// ```
    /// # let folder = tempfile::tempdir()?;
    /// use lsm_tree::{AbstractTree, Config, Tree};
    ///
    /// let tree = Config::new(folder).open()?;
    ///
    /// tree.insert("a", "abc", 0);
    /// let snapshot = tree.snapshot(1);
    ///
    /// tree.insert("f", "abc", 1);
    /// tree.insert("g", "abc", 2);
    ///
    /// assert_eq!(1, snapshot.range("a"..="f").count());
    /// #
    /// # Ok::<(), lsm_tree::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn range<K: AsRef<[u8]>, R: RangeBounds<K>>(
        &self,
        range: R,
    ) -> impl DoubleEndedIterator<Item = crate::Result<KvPair>> {
        self.tree.range_with_seqno(range, self.seqno, None)
    }

    /// Returns an iterator over a prefixed set of items in the snapshot.
    ///
    /// Avoid using an empty prefix as it may scan a lot of items (unless limited).
    ///
    /// # Examples
    ///
    /// ```
    /// # let folder = tempfile::tempdir()?;
    /// use lsm_tree::{AbstractTree, Config, Tree};
    ///
    /// let tree = Config::new(folder).open()?;
    ///
    /// tree.insert("a", "abc", 0);
    /// tree.insert("ab", "abc", 1);
    /// let snapshot = tree.snapshot(2);
    ///
    /// tree.insert("abc", "abc", 2);
    ///
    /// assert_eq!(2, snapshot.prefix("a").count());
    /// #
    /// # Ok::<(), lsm_tree::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn prefix<K: AsRef<[u8]>>(
        &self,
        prefix: K,
    ) -> impl DoubleEndedIterator<Item = crate::Result<KvPair>> {
        self.tree.prefix_with_seqno(prefix, self.seqno, None)
    }

    /// Returns the first key-value pair in the snapshot.
    /// The key in this pair is the minimum key in the snapshot.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsm_tree::Error as TreeError;
    /// use lsm_tree::{AbstractTree, Config, Tree};
    ///
    /// # let folder = tempfile::tempdir()?;
    /// let tree = Config::new(folder).open()?;
    ///
    /// tree.insert("5", "abc", 0);
    /// tree.insert("3", "abc", 1);
    /// let snapshot = tree.snapshot(2);
    ///
    /// tree.insert("1", "abc", 2);
    ///
    /// let (key, _) = snapshot.first_key_value()?.expect("item should exist");
    /// assert_eq!(&*key, "3".as_bytes());
    /// #
    /// # Ok::<(), TreeError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn first_key_value(&self) -> crate::Result<Option<(UserKey, UserValue)>> {
        self.iter().next().transpose()
    }

    /// Returns the las key-value pair in the snapshot.
    /// The key in this pair is the maximum key in the snapshot.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsm_tree::Error as TreeError;
    /// use lsm_tree::{AbstractTree, Config, Tree};
    ///
    /// # let folder = tempfile::tempdir()?;
    /// let tree = Config::new(folder).open()?;
    ///
    /// tree.insert("1", "abc", 0);
    /// tree.insert("3", "abc", 1);
    /// let snapshot = tree.snapshot(2);
    ///
    /// tree.insert("5", "abc", 2);
    ///
    /// let (key, _) = snapshot.last_key_value()?.expect("item should exist");
    /// assert_eq!(&*key, "3".as_bytes());
    /// #
    /// # Ok::<(), TreeError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn last_key_value(&self) -> crate::Result<Option<(UserKey, UserValue)>> {
        self.iter().next_back().transpose()
    }

    /// Returns `true` if the snapshot contains the specified key.
    ///
    /// # Examples
    ///
    /// ```
    /// # let folder = tempfile::tempdir()?;
    /// use lsm_tree::{AbstractTree, Config, Tree};
    ///
    /// let tree = Config::new(folder).open()?;
    /// let snapshot = tree.snapshot(0);
    ///
    /// assert!(!snapshot.contains_key("a")?);
    ///
    /// tree.insert("a", "abc", 0);
    /// assert!(!snapshot.contains_key("a")?);
    /// #
    /// # Ok::<(), lsm_tree::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn contains_key<K: AsRef<[u8]>>(&self, key: K) -> crate::Result<bool> {
        self.get(key).map(|x| x.is_some())
    }

    /// Returns `true` if the snapshot is empty.
    ///
    /// This operation has O(1) complexity.
    ///
    /// # Examples
    ///
    /// ```
    /// # let folder = tempfile::tempdir()?;
    /// use lsm_tree::{AbstractTree, Config, Tree};
    ///
    /// let tree = Config::new(folder).open()?;
    /// let snapshot = tree.snapshot(0);
    ///
    /// assert!(snapshot.is_empty()?);
    ///
    /// tree.insert("a", "abc", 0);
    /// assert!(snapshot.is_empty()?);
    /// #
    /// # Ok::<(), lsm_tree::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn is_empty(&self) -> crate::Result<bool> {
        self.first_key_value().map(|x| x.is_none())
    }

    /// Scans the entire snapshot, returning the amount of items.
    ///
    /// ###### Caution
    ///
    /// This operation scans the entire tree: O(n) complexity!
    ///
    /// Never, under any circumstances, use .`len()` == 0 to check
    /// if the snapshot is empty, use [`Snapshot::is_empty`] instead.
    ///
    /// # Examples
    ///
    /// ```
    /// # use lsm_tree::Error as TreeError;
    /// use lsm_tree::{AbstractTree, Config, Tree};
    ///
    /// # let folder = tempfile::tempdir()?;
    /// let tree = Config::new(folder).open()?;
    /// let snapshot = tree.snapshot(0);
    ///
    /// assert_eq!(snapshot.len()?, 0);
    /// tree.insert("1", "abc", 0);
    /// tree.insert("3", "abc", 1);
    /// tree.insert("5", "abc", 2);
    /// assert_eq!(snapshot.len()?, 0);
    /// #
    /// # Ok::<(), TreeError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Will return `Err` if an IO error occurs.
    pub fn len(&self) -> crate::Result<usize> {
        let mut count = 0;

        for item in self.iter() {
            let _ = item?;
            count += 1;
        }

        Ok(count)
    }
}

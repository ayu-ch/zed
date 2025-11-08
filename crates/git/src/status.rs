use crate::{Oid, repository::RepoPath};
use anyhow::{Result, anyhow};
use collections::HashMap;
use gpui::SharedString;
use serde::{Deserialize, Serialize};
use std::{str::FromStr, sync::Arc};
use util::{ResultExt, rel_path::RelPath};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileStatus {
    Untracked,
    Ignored,
    Unmerged(UnmergedStatus),
    Tracked(TrackedStatus),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UnmergedStatus {
    pub first_head: UnmergedStatusCode,
    pub second_head: UnmergedStatusCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnmergedStatusCode {
    Added,
    Deleted,
    Updated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrackedStatus {
    pub index_status: StatusCode,
    pub worktree_status: StatusCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StatusCode {
    Modified,
    TypeChanged,
    Added,
    Deleted,
    Renamed,
    Copied,
    Unmodified,
}

impl From<UnmergedStatus> for FileStatus {
    fn from(value: UnmergedStatus) -> Self {
        FileStatus::Unmerged(value)
    }
}

impl From<TrackedStatus> for FileStatus {
    fn from(value: TrackedStatus) -> Self {
        FileStatus::Tracked(value)
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum StageStatus {
    Staged,
    Unstaged,
    PartiallyStaged,
}

impl StageStatus {
    pub fn is_fully_staged(&self) -> bool {
        matches!(self, StageStatus::Staged)
    }

    pub fn is_fully_unstaged(&self) -> bool {
        matches!(self, StageStatus::Unstaged)
    }

    pub fn has_staged(&self) -> bool {
        matches!(self, StageStatus::Staged | StageStatus::PartiallyStaged)
    }

    pub fn has_unstaged(&self) -> bool {
        matches!(self, StageStatus::Unstaged | StageStatus::PartiallyStaged)
    }

    pub fn as_bool(self) -> Option<bool> {
        match self {
            StageStatus::Staged => Some(true),
            StageStatus::Unstaged => Some(false),
            StageStatus::PartiallyStaged => None,
        }
    }
}

impl FileStatus {
    pub const fn worktree(worktree_status: StatusCode) -> Self {
        FileStatus::Tracked(TrackedStatus {
            index_status: StatusCode::Unmodified,
            worktree_status,
        })
    }

    pub const fn index(index_status: StatusCode) -> Self {
        FileStatus::Tracked(TrackedStatus {
            worktree_status: StatusCode::Unmodified,
            index_status,
        })
    }

    /// Generate a FileStatus Code from a byte pair, as described in
    /// https://git-scm.com/docs/git-status#_output
    ///
    /// NOTE: That instead of '', we use ' ' to denote no change
    fn from_bytes(bytes: [u8; 2]) -> anyhow::Result<Self> {
        let status = match bytes {
            [b'?', b'?'] => FileStatus::Untracked,
            [b'!', b'!'] => FileStatus::Ignored,
            [b'A', b'A'] => UnmergedStatus {
                first_head: UnmergedStatusCode::Added,
                second_head: UnmergedStatusCode::Added,
            }
            .into(),
            [b'D', b'D'] => UnmergedStatus {
                first_head: UnmergedStatusCode::Added,
                second_head: UnmergedStatusCode::Added,
            }
            .into(),
            [x, b'U'] => UnmergedStatus {
                first_head: UnmergedStatusCode::from_byte(x)?,
                second_head: UnmergedStatusCode::Updated,
            }
            .into(),
            [b'U', y] => UnmergedStatus {
                first_head: UnmergedStatusCode::Updated,
                second_head: UnmergedStatusCode::from_byte(y)?,
            }
            .into(),
            [x, y] => TrackedStatus {
                index_status: StatusCode::from_byte(x)?,
                worktree_status: StatusCode::from_byte(y)?,
            }
            .into(),
        };
        Ok(status)
    }

    pub fn staging(self) -> StageStatus {
        match self {
            FileStatus::Untracked | FileStatus::Ignored | FileStatus::Unmerged { .. } => {
                StageStatus::Unstaged
            }
            FileStatus::Tracked(tracked) => match (tracked.index_status, tracked.worktree_status) {
                (StatusCode::Unmodified, _) => StageStatus::Unstaged,
                (_, StatusCode::Unmodified) => StageStatus::Staged,
                _ => StageStatus::PartiallyStaged,
            },
        }
    }

    pub fn is_conflicted(self) -> bool {
        matches!(self, FileStatus::Unmerged { .. })
    }

    pub fn is_ignored(self) -> bool {
        matches!(self, FileStatus::Ignored)
    }

    pub fn has_changes(&self) -> bool {
        self.is_modified()
            || self.is_created()
            || self.is_deleted()
            || self.is_untracked()
            || self.is_conflicted()
    }

    pub fn is_modified(self) -> bool {
        match self {
            FileStatus::Tracked(tracked) => matches!(
                (tracked.index_status, tracked.worktree_status),
                (StatusCode::Modified, _) | (_, StatusCode::Modified)
            ),
            _ => false,
        }
    }

    pub fn is_created(self) -> bool {
        match self {
            FileStatus::Tracked(tracked) => matches!(
                (tracked.index_status, tracked.worktree_status),
                (StatusCode::Added, _) | (_, StatusCode::Added)
            ),
            FileStatus::Untracked => true,
            _ => false,
        }
    }

    pub fn is_deleted(self) -> bool {
        let FileStatus::Tracked(tracked) = self else {
            return false;
        };
        tracked.index_status == StatusCode::Deleted && tracked.worktree_status != StatusCode::Added
            || tracked.worktree_status == StatusCode::Deleted
    }

    pub fn is_untracked(self) -> bool {
        matches!(self, FileStatus::Untracked)
    }

    pub fn is_renamed(self) -> bool {
        match self {
            FileStatus::Tracked(tracked) => matches!(
                (tracked.index_status, tracked.worktree_status),
                (StatusCode::Renamed, _) | (_, StatusCode::Renamed)
            ),
            _ => false,
        }
    }

    pub fn summary(self) -> GitSummary {
        match self {
            FileStatus::Ignored => GitSummary::UNCHANGED,
            FileStatus::Untracked => GitSummary::UNTRACKED,
            FileStatus::Unmerged(_) => GitSummary::CONFLICT,
            FileStatus::Tracked(TrackedStatus {
                index_status,
                worktree_status,
            }) => GitSummary {
                index: index_status.to_summary(),
                worktree: worktree_status.to_summary(),
                conflict: 0,
                untracked: 0,
                count: 1,
            },
        }
    }
}

impl StatusCode {
    fn from_byte(byte: u8) -> anyhow::Result<Self> {
        match byte {
            b'M' => Ok(StatusCode::Modified),
            b'T' => Ok(StatusCode::TypeChanged),
            b'A' => Ok(StatusCode::Added),
            b'D' => Ok(StatusCode::Deleted),
            b'R' => Ok(StatusCode::Renamed),
            b'C' => Ok(StatusCode::Copied),
            b' ' => Ok(StatusCode::Unmodified),
            _ => anyhow::bail!("Invalid status code: {byte}"),
        }
    }

    fn to_summary(self) -> TrackedSummary {
        match self {
            StatusCode::Modified | StatusCode::TypeChanged | StatusCode::Renamed => TrackedSummary {
                modified: 1,
                ..TrackedSummary::UNCHANGED
            },
            StatusCode::Added => TrackedSummary {
                added: 1,
                ..TrackedSummary::UNCHANGED
            },
            StatusCode::Deleted => TrackedSummary {
                deleted: 1,
                ..TrackedSummary::UNCHANGED
            },
            StatusCode::Copied | StatusCode::Unmodified => {
                TrackedSummary::UNCHANGED
            }
        }
    }

    pub fn index(self) -> FileStatus {
        FileStatus::Tracked(TrackedStatus {
            index_status: self,
            worktree_status: StatusCode::Unmodified,
        })
    }

    pub fn worktree(self) -> FileStatus {
        FileStatus::Tracked(TrackedStatus {
            index_status: StatusCode::Unmodified,
            worktree_status: self,
        })
    }
}

impl UnmergedStatusCode {
    fn from_byte(byte: u8) -> anyhow::Result<Self> {
        match byte {
            b'A' => Ok(UnmergedStatusCode::Added),
            b'D' => Ok(UnmergedStatusCode::Deleted),
            b'U' => Ok(UnmergedStatusCode::Updated),
            _ => anyhow::bail!("Invalid unmerged status code: {byte}"),
        }
    }
}

#[derive(Clone, Debug, Default, Copy, PartialEq, Eq)]
pub struct TrackedSummary {
    pub added: usize,
    pub modified: usize,
    pub deleted: usize,
}

impl TrackedSummary {
    pub const UNCHANGED: Self = Self {
        added: 0,
        modified: 0,
        deleted: 0,
    };

    pub const ADDED: Self = Self {
        added: 1,
        modified: 0,
        deleted: 0,
    };

    pub const MODIFIED: Self = Self {
        added: 0,
        modified: 1,
        deleted: 0,
    };

    pub const DELETED: Self = Self {
        added: 0,
        modified: 0,
        deleted: 1,
    };
}

impl std::ops::AddAssign for TrackedSummary {
    fn add_assign(&mut self, rhs: Self) {
        self.added += rhs.added;
        self.modified += rhs.modified;
        self.deleted += rhs.deleted;
    }
}

impl std::ops::Add for TrackedSummary {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        TrackedSummary {
            added: self.added + rhs.added,
            modified: self.modified + rhs.modified,
            deleted: self.deleted + rhs.deleted,
        }
    }
}

impl std::ops::Sub for TrackedSummary {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        TrackedSummary {
            added: self.added - rhs.added,
            modified: self.modified - rhs.modified,
            deleted: self.deleted - rhs.deleted,
        }
    }
}

#[derive(Clone, Debug, Default, Copy, PartialEq, Eq)]
pub struct GitSummary {
    pub index: TrackedSummary,
    pub worktree: TrackedSummary,
    pub conflict: usize,
    pub untracked: usize,
    pub count: usize,
}

impl GitSummary {
    pub const CONFLICT: Self = Self {
        conflict: 1,
        count: 1,
        ..Self::UNCHANGED
    };

    pub const UNTRACKED: Self = Self {
        untracked: 1,
        count: 1,
        ..Self::UNCHANGED
    };

    pub const UNCHANGED: Self = Self {
        index: TrackedSummary::UNCHANGED,
        worktree: TrackedSummary::UNCHANGED,
        conflict: 0,
        untracked: 0,
        count: 0,
    };
}

impl From<FileStatus> for GitSummary {
    fn from(status: FileStatus) -> Self {
        status.summary()
    }
}

impl sum_tree::ContextLessSummary for GitSummary {
    fn zero() -> Self {
        Default::default()
    }

    fn add_summary(&mut self, rhs: &Self) {
        *self += *rhs;
    }
}

impl std::ops::Add<Self> for GitSummary {
    type Output = Self;

    fn add(mut self, rhs: Self) -> Self {
        self += rhs;
        self
    }
}

impl std::ops::AddAssign for GitSummary {
    fn add_assign(&mut self, rhs: Self) {
        self.index += rhs.index;
        self.worktree += rhs.worktree;
        self.conflict += rhs.conflict;
        self.untracked += rhs.untracked;
        self.count += rhs.count;
    }
}

impl std::ops::Sub for GitSummary {
    type Output = GitSummary;

    fn sub(self, rhs: Self) -> Self::Output {
        GitSummary {
            index: self.index - rhs.index,
            worktree: self.worktree - rhs.worktree,
            conflict: self.conflict - rhs.conflict,
            untracked: self.untracked - rhs.untracked,
            count: self.count - rhs.count,
        }
    }
}

#[derive(Clone, Debug)]
pub struct GitStatus {
    pub entries: Arc<[(RepoPath, FileStatus)]>,
}

impl FromStr for GitStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let mut parts = s.split('\0').peekable();
        let mut entries = Vec::new();
        
        while let Some(entry) = parts.next() {
            if entry.is_empty() {
                continue;
            }
            
            let sep = match entry.get(2..3) {
                Some(s) => s,
                None => continue,
            };
            
            if sep != " " {
                continue;
            }
            
            let status_bytes = entry.as_bytes()[0..2].try_into().unwrap();
            let status = match FileStatus::from_bytes(status_bytes).log_err() {
                Some(s) => s,
                None => continue,
            };
            
            // Check if this is a rename or copy (which have two paths)
            let is_rename_or_copy = matches!(status_bytes, [b'R', _] | [_, b'R'] | [b'C', _] | [_, b'C']);
            
            if is_rename_or_copy {
                // For renames/copies, format is: "R100 old_path\0new_path\0"
                // We already consumed the first part, now get the old and new paths
                let _old_path = parts.next(); // Skip old path for now
                let new_path = match parts.next() {
                    Some(p) => p,
                    None => continue,
                };
                
                // The git status output includes untracked directories as well as untracked files.
                // We do our own processing to compute the "summary" status of each directory,
                // so just skip any directories in the output, since they'll otherwise interfere
                // with our handling of nested repositories.
                if new_path.ends_with('/') {
                    continue;
                }
                
                // git-status outputs `/`-delimited repo paths, even on Windows.
                let path = match RelPath::unix(new_path).log_err() {
                    Some(p) => RepoPath(p.into()),
                    None => continue,
                };
                entries.push((path, status));
            } else {
                // Normal status entry
                let path = &entry[3..];
                
                // The git status output includes untracked directories as well as untracked files.
                // We do our own processing to compute the "summary" status of each directory,
                // so just skip any directories in the output, since they'll otherwise interfere
                // with our handling of nested repositories.
                if path.ends_with('/') {
                    continue;
                }
                
                // git-status outputs `/`-delimited repo paths, even on Windows.
                let path = match RelPath::unix(path).log_err() {
                    Some(p) => RepoPath(p.into()),
                    None => continue,
                };
                entries.push((path, status));
            }
        }
        entries.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        // When a file exists in HEAD, is deleted in the index, and exists again in the working copy,
        // git produces two lines for it, one reading `D ` (deleted in index, unmodified in working copy)
        // and the other reading `??` (untracked). Merge these two into the equivalent of `DA`.
        entries.dedup_by(|(a, a_status), (b, b_status)| {
            const INDEX_DELETED: FileStatus = FileStatus::index(StatusCode::Deleted);
            if a.ne(&b) {
                return false;
            }
            match (*a_status, *b_status) {
                (INDEX_DELETED, FileStatus::Untracked) | (FileStatus::Untracked, INDEX_DELETED) => {
                    *b_status = TrackedStatus {
                        index_status: StatusCode::Deleted,
                        worktree_status: StatusCode::Added,
                    }
                    .into();
                }
                _ => panic!("Unexpected duplicated status entries: {a_status:?} and {b_status:?}"),
            }
            true
        });
        Ok(Self {
            entries: entries.into(),
        })
    }
}

impl Default for GitStatus {
    fn default() -> Self {
        Self {
            entries: Arc::new([]),
        }
    }
}

pub enum DiffTreeType {
    MergeBase {
        base: SharedString,
        head: SharedString,
    },
    Since {
        base: SharedString,
        head: SharedString,
    },
}

impl DiffTreeType {
    pub fn base(&self) -> &SharedString {
        match self {
            DiffTreeType::MergeBase { base, .. } => base,
            DiffTreeType::Since { base, .. } => base,
        }
    }

    pub fn head(&self) -> &SharedString {
        match self {
            DiffTreeType::MergeBase { head, .. } => head,
            DiffTreeType::Since { head, .. } => head,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct TreeDiff {
    pub entries: HashMap<RepoPath, TreeDiffStatus>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TreeDiffStatus {
    Added,
    Modified { old: Oid },
    Deleted { old: Oid },
}

impl FromStr for TreeDiff {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let mut fields = s.split('\0');
        let mut parsed = HashMap::default();
        while let Some((status, path)) = fields.next().zip(fields.next()) {
            let path = RepoPath(RelPath::unix(path)?.into());

            let mut fields = status.split(" ").skip(2);
            let old_sha = fields
                .next()
                .ok_or_else(|| anyhow!("expected to find old_sha"))?
                .to_owned()
                .parse()?;
            let _new_sha = fields
                .next()
                .ok_or_else(|| anyhow!("expected to find new_sha"))?;
            let status = fields
                .next()
                .and_then(|s| {
                    if s.len() == 1 {
                        s.as_bytes().first()
                    } else {
                        None
                    }
                })
                .ok_or_else(|| anyhow!("expected to find status"))?;

            let result = match StatusCode::from_byte(*status)? {
                StatusCode::Modified => TreeDiffStatus::Modified { old: old_sha },
                StatusCode::Added => TreeDiffStatus::Added,
                StatusCode::Deleted => TreeDiffStatus::Deleted { old: old_sha },
                _status => continue,
            };

            parsed.insert(path, result);
        }

        Ok(Self { entries: parsed })
    }
}

#[cfg(test)]
mod tests {

    use crate::{
        repository::RepoPath,
        status::{FileStatus, GitStatus, StatusCode, TrackedStatus, TreeDiff, TreeDiffStatus},
    };

    #[test]
    fn test_git_status_with_renames() {
        // Test parsing git status output with renamed files
        // Format: "R100 old_name\0new_name\0"
        let input = "R  old_file.txt\0new_file.txt\0M  modified.txt\0?? untracked.txt\0";
        
        let status: GitStatus = input.parse().unwrap();
        
        assert_eq!(status.entries.len(), 3);
        
        // Check that the renamed file shows up with the new name
        let renamed_entry = status.entries.iter().find(|(path, _)| path.0.as_str() == "new_file.txt");
        assert!(renamed_entry.is_some());
        let (_, file_status) = renamed_entry.unwrap();
        assert!(file_status.is_renamed());
        
        // Check modified file
        let modified_entry = status.entries.iter().find(|(path, _)| path.0.as_str() == "modified.txt");
        assert!(modified_entry.is_some());
        let (_, file_status) = modified_entry.unwrap();
        assert!(file_status.is_modified());
        
        // Check untracked file
        let untracked_entry = status.entries.iter().find(|(path, _)| path.0.as_str() == "untracked.txt");
        assert!(untracked_entry.is_some());
        let (_, file_status) = untracked_entry.unwrap();
        assert!(file_status.is_untracked());
    }

    #[test]
    fn test_tree_diff_parsing() {
        let input = ":000000 100644 0000000000000000000000000000000000000000 0062c311b8727c3a2e3cd7a41bc9904feacf8f98 A\x00.zed/settings.json\x00".to_owned() +
            ":100644 000000 bb3e9ed2e97a8c02545bae243264d342c069afb3 0000000000000000000000000000000000000000 D\x00README.md\x00" +
            ":100644 100644 42f097005a1f21eb2260fad02ec8c991282beee8 a437d85f63bb8c62bd78f83f40c506631fabf005 M\x00parallel.go\x00";

        let output: TreeDiff = input.parse().unwrap();
        assert_eq!(
            output,
            TreeDiff {
                entries: [
                    (
                        RepoPath::new(".zed/settings.json").unwrap(),
                        TreeDiffStatus::Added,
                    ),
                    (
                        RepoPath::new("README.md").unwrap(),
                        TreeDiffStatus::Deleted {
                            old: "bb3e9ed2e97a8c02545bae243264d342c069afb3".parse().unwrap()
                        }
                    ),
                    (
                        RepoPath::new("parallel.go").unwrap(),
                        TreeDiffStatus::Modified {
                            old: "42f097005a1f21eb2260fad02ec8c991282beee8".parse().unwrap(),
                        }
                    ),
                ]
                .into_iter()
                .collect()
            }
        )
    }
}

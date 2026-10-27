//! Block-level patches between two view models (`spec/schema/patch.schema.json`).
//!
//! A top-level block of a story (paragraph, table or image block) is the unit: a change
//! anywhere inside a table upserts the whole table. Comments, change sets, meta, sections and
//! new stories travel whole.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::vm::{Block, Comment, Meta, Section, Story, ViewModel, VmAsset};
use super::{ChangeSetInfo, SPEC_VERSION};
use crate::{Error, Result};

/// Where a block goes: after sibling `after` (`None` = first) in `story`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub id: String,
    pub story: String,
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Upsert {
    pub story: String,
    pub after: Option<String>,
    pub block: Block,
}

/// What turns view model `base_version` into view model `version`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Patch {
    #[serde(rename = "spec_version")]
    pub spec_version: String,
    #[serde(rename = "base_version")]
    pub base_version: u64,
    pub version: u64,
    pub removes: Vec<String>,
    pub moves: Vec<Position>,
    pub upserts: Vec<Upsert>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub story_removes: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stories: BTreeMap<String, Story>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<Comment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comment_removes: Vec<String>,
    /// Every comment id in its new order, when applying `comments` and `commentRemoves` (which
    /// append new comments) would leave the list in another order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_order: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub change_sets: Vec<ChangeSetInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<Section>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assets: BTreeMap<String, VmAsset>,
}

impl Patch {
    /// Nothing changed.
    pub fn is_empty(&self) -> bool {
        self.removes.is_empty()
            && self.moves.is_empty()
            && self.upserts.is_empty()
            && self.story_removes.is_empty()
            && self.stories.is_empty()
            && self.comments.is_empty()
            && self.comment_removes.is_empty()
            && self.comment_order.is_none()
            && self.change_sets.is_empty()
            && self.meta.is_none()
            && self.sections.is_none()
            && self.assets.is_empty()
    }

    /// Apply to `vm` the way a viewer does (the order in `patch.schema.json`). Fails when
    /// `vm.version` isn't `base_version` or an id doesn't resolve.
    pub fn apply(&self, vm: &mut ViewModel) -> Result<()> {
        let bad = |m: String| Error::Anchor(format!("patch: {m}"));
        if vm.version != self.base_version {
            return Err(bad(format!("base version {} != view model version {}", self.base_version, vm.version)));
        }
        let gone: HashSet<&str> = self.removes.iter().map(String::as_str).collect();
        for b in vm.story_keys() {
            if let Some(list) = vm.story_blocks_mut(&b) {
                list.retain(|x| !gone.contains(x.id()));
            }
        }
        for s in &self.story_removes {
            vm.stories.remove(s);
        }
        for m in &self.moves {
            let blk = take_block(vm, &m.id).ok_or_else(|| bad(format!("move of unknown block {}", m.id)))?;
            insert_after(vm, &m.story, m.after.as_deref(), blk).map_err(bad)?;
        }
        for u in &self.upserts {
            let id = u.block.id();
            if let Some(slot) = find_block_mut(vm, id) {
                *slot = u.block.clone();
            } else {
                insert_after(vm, &u.story, u.after.as_deref(), u.block.clone()).map_err(bad)?;
            }
        }
        for (k, s) in &self.stories {
            vm.stories.insert(k.clone(), s.clone());
        }
        for c in &self.comments {
            match vm.comments.iter_mut().find(|x| x.id == c.id) {
                Some(x) => *x = c.clone(),
                None => vm.comments.push(c.clone()),
            }
        }
        vm.comments.retain(|c| !self.comment_removes.contains(&c.id));
        if let Some(order) = &self.comment_order {
            let rank: HashMap<&str, usize> = order.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
            vm.comments.sort_by_key(|c| rank.get(c.id.as_str()).copied().unwrap_or(usize::MAX));
        }
        for cs in &self.change_sets {
            match vm.change_sets.iter_mut().find(|x| x.id == cs.id) {
                Some(x) => *x = cs.clone(),
                None => vm.change_sets.push(cs.clone()),
            }
        }
        if let Some(m) = &self.meta {
            vm.meta = m.clone();
        }
        if let Some(s) = &self.sections {
            vm.sections = s.clone();
        }
        for (k, a) in &self.assets {
            vm.assets.insert(k.clone(), a.clone());
        }
        vm.version = self.version;
        Ok(())
    }
}

impl ViewModel {
    /// `b`, every story key and every comment id.
    fn story_keys(&self) -> Vec<String> {
        let mut v = vec!["b".to_owned()];
        v.extend(self.stories.keys().cloned());
        v.extend(self.comments.iter().map(|c| c.id.clone()));
        v
    }
}

fn take_block(vm: &mut ViewModel, id: &str) -> Option<Block> {
    for k in vm.story_keys() {
        let list = vm.story_blocks_mut(&k)?;
        if let Some(i) = list.iter().position(|b| b.id() == id) {
            return Some(list.remove(i));
        }
    }
    None
}

fn find_block_mut<'v>(vm: &'v mut ViewModel, id: &str) -> Option<&'v mut Block> {
    let keys = vm.story_keys();
    let k = keys.iter().find(|k| vm.story_blocks(k).is_some_and(|l| l.iter().any(|b| b.id() == id)))?;
    vm.story_blocks_mut(k)?.iter_mut().find(|b| b.id() == id)
}

fn insert_after(vm: &mut ViewModel, story: &str, after: Option<&str>, b: Block) -> std::result::Result<(), String> {
    let list = vm.story_blocks_mut(story).ok_or_else(|| format!("unknown story {story}"))?;
    let at = match after {
        None => 0,
        Some(a) => list.iter().position(|x| x.id() == a).ok_or_else(|| format!("unknown sibling {a} in {story}"))? + 1,
    };
    list.insert(at, b);
    Ok(())
}

/// Indices (into `seq`) of one longest strictly increasing subsequence.
fn lis(seq: &[usize]) -> HashSet<usize> {
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; seq.len()];
    for (i, &v) in seq.iter().enumerate() {
        let k = tails.partition_point(|&t| seq[t] < v);
        if k > 0 {
            prev[i] = tails[k - 1];
        }
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    let mut out = HashSet::new();
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        out.insert(i);
        cur = (prev[i] != usize::MAX).then_some(prev[i]);
    }
    out
}

/// The patch from `before` to `after`. `changed_anchors` (an op result's `changed`) forces
/// the blocks holding them (and the comments they name) into the patch even if they compare
/// equal.
pub fn patch(before: &ViewModel, after: &ViewModel, changed_anchors: &[&str]) -> Patch {
    let forced: HashSet<&str> = changed_anchors.iter().copied().collect();
    // Block-level stories: the body and the header/footer/note/text-box stories.
    let story_lists = |vm: &ViewModel| -> Vec<(String, Vec<Block>)> {
        let mut v = vec![("b".to_owned(), vm.blocks.clone())];
        v.extend(vm.stories.iter().map(|(k, s)| (k.clone(), s.blocks.clone())));
        v
    };
    let b_lists = story_lists(before);
    let a_lists = story_lists(after);
    let mut b_where: HashMap<&str, (&str, usize, &Block)> = HashMap::new();
    for (k, l) in &b_lists {
        for (i, blk) in l.iter().enumerate() {
            b_where.insert(blk.id(), (k.as_str(), i, blk));
        }
    }
    let new_stories: BTreeMap<String, Story> = after
        .stories
        .iter()
        .filter(|(k, _)| !before.stories.contains_key(*k))
        .map(|(k, s)| (k.clone(), s.clone()))
        .collect();
    let story_removes: Vec<String> =
        before.stories.keys().filter(|k| !after.stories.contains_key(*k)).cloned().collect();

    let mut a_ids: HashSet<&str> = HashSet::new();
    for (_, l) in &a_lists {
        for blk in l {
            a_ids.insert(blk.id());
        }
    }
    let removes: Vec<String> = b_lists
        .iter()
        .filter(|(k, _)| !story_removes.contains(k))
        .flat_map(|(_, l)| l.iter().map(Block::id))
        .filter(|id| !a_ids.contains(id))
        .map(str::to_owned)
        .collect();

    let mut moves = Vec::new();
    let mut upserts = Vec::new();
    for (k, l) in &a_lists {
        if new_stories.contains_key(k) {
            continue;
        }
        // Blocks that were already in this story, in their new order: the ones off the
        // longest run of preserved order move.
        let kept: Vec<(usize, usize)> = l
            .iter()
            .enumerate()
            .filter_map(|(ai, blk)| b_where.get(blk.id()).filter(|(bk, _, _)| bk == k).map(|(_, bi, _)| (ai, *bi)))
            .collect();
        let stay = lis(&kept.iter().map(|&(_, bi)| bi).collect::<Vec<_>>());
        let stay: HashSet<usize> = stay.into_iter().map(|j| kept[j].0).collect();
        let mut last_existing: Option<&str> = None;
        let mut prev: Option<&str> = None;
        for (ai, blk) in l.iter().enumerate() {
            let id = blk.id();
            match b_where.get(id) {
                Some((bk, _, old)) => {
                    let moved = bk != k || !stay.contains(&ai);
                    if moved {
                        moves.push(Position {
                            id: id.to_owned(),
                            story: k.clone(),
                            after: last_existing.map(str::to_owned),
                        });
                    }
                    let mut anchors = Vec::new();
                    blk.anchors(&mut anchors);
                    if *old != blk || anchors.iter().any(|a| forced.contains(a.as_str())) {
                        upserts.push(Upsert { story: k.clone(), after: prev.map(str::to_owned), block: blk.clone() });
                    }
                    last_existing = Some(id);
                }
                None => {
                    upserts.push(Upsert { story: k.clone(), after: prev.map(str::to_owned), block: blk.clone() });
                }
            }
            prev = Some(id);
        }
    }

    let b_comments: HashMap<&str, &Comment> = before.comments.iter().map(|c| (c.id.as_str(), c)).collect();
    let comments: Vec<Comment> = after
        .comments
        .iter()
        .filter(|c| {
            let mut forced_here = forced.contains(c.id.as_str());
            if !forced_here {
                let mut anchors = Vec::new();
                for b in &c.blocks {
                    b.anchors(&mut anchors);
                }
                forced_here = anchors.iter().any(|a| forced.contains(a.as_str()));
            }
            forced_here || b_comments.get(c.id.as_str()).is_none_or(|o| *o != *c)
        })
        .cloned()
        .collect();
    let comment_removes: Vec<String> =
        before.comments.iter().filter(|c| !after.comments.iter().any(|a| a.id == c.id)).map(|c| c.id.clone()).collect();
    // New comments are appended; send the order when that isn't the new one.
    let mut naive: Vec<&str> =
        before.comments.iter().map(|c| c.id.as_str()).filter(|id| !comment_removes.iter().any(|r| r == id)).collect();
    naive.extend(comments.iter().map(|c| c.id.as_str()).filter(|id| !b_comments.contains_key(id)));
    let after_order: Vec<&str> = after.comments.iter().map(|c| c.id.as_str()).collect();
    let comment_order = (naive != after_order).then(|| after_order.iter().map(|&s| s.to_owned()).collect());
    let change_sets: Vec<ChangeSetInfo> = after
        .change_sets
        .iter()
        .filter(|cs| before.change_sets.iter().find(|o| o.id == cs.id).is_none_or(|o| o != *cs))
        .cloned()
        .collect();
    let assets: BTreeMap<String, VmAsset> = after
        .assets
        .iter()
        .filter(|(k, _)| !before.assets.contains_key(*k))
        .map(|(k, a)| (k.clone(), a.clone()))
        .collect();

    Patch {
        spec_version: SPEC_VERSION.into(),
        base_version: before.version,
        version: after.version,
        removes,
        moves,
        upserts,
        story_removes,
        stories: new_stories,
        comments,
        comment_removes,
        comment_order,
        change_sets,
        meta: (before.meta != after.meta).then(|| after.meta.clone()),
        sections: (before.sections != after.sections).then(|| after.sections.clone()),
        assets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lis_basic() {
        let s = lis(&[0, 3, 1, 2, 4]);
        let mut v: Vec<_> = s.into_iter().collect();
        v.sort();
        assert_eq!(v, vec![0, 2, 3, 4]);
        assert!(lis(&[]).is_empty());
    }
}

//! Bounded contextual structural equality. Only Marker leaves consult Session;
//! no copied position mirror, callback, mutable Context, or escaping owner borrow.
use crate::core::{gc::Context, object::{Object, ObjectType}};
use anyhow::{Result, ensure};
use std::collections::HashSet;

enum Task<'a> { Pair(Object<'a>, Object<'a>, usize), Leave((usize, usize)) }
pub(crate) fn equal(left: Object<'_>, right: Object<'_>, cx: &Context) -> Result<bool> {
    let mut pending = vec![Task::Pair(left, right, 0)];
    let mut active = HashSet::new();
    let mut completed = HashSet::new();
    let mut work = 0usize;
    while let Some(task) = pending.pop() {
        let (left, right, depth) = match task {
            Task::Leave(pair) => { active.remove(&pair); completed.insert(pair); continue; },
            Task::Pair(left, right, depth) => (left, right, depth),
        };
        work += 1;
        ensure!(work <= 100_000 && depth <= 512, "Contextual equality traversal limit exceeded");
        if let (ObjectType::Marker(a), ObjectType::Marker(b)) = (left.untag(), right.untag()) {
            if crate::marker::location(a, cx)? != crate::marker::location(b, cx)? { return Ok(false); }
            continue;
        }
        if left.ptr_eq(right) { continue; }
        let mut children = Vec::new();
        match (left.untag(), right.untag()) {
            (ObjectType::Cons(a), ObjectType::Cons(b)) => children.extend([(a.car(), b.car()), (a.cdr(), b.cdr())]),
            (ObjectType::Vec(a), ObjectType::Vec(b)) => {
                if a.len() != b.len() { return Ok(false); }
                ensure!(a.len() <= 100_000 - work, "Contextual equality edge limit exceeded");
                children.try_reserve_exact(a.len())?;
                children.extend(a.iter().zip(b.iter()).map(|(a,b)| (a.get(), b.get())));
            },
            (ObjectType::Record(a), ObjectType::Record(b)) => {
                if a.len() != b.len() { return Ok(false); }
                ensure!(a.len() <= 100_000 - work, "Contextual equality edge limit exceeded");
                children.try_reserve_exact(a.len())?;
                children.extend(a.iter().zip(b.iter()).map(|(a,b)| (a.get(), b.get())));
            },
            (ObjectType::ByteFn(a), ObjectType::ByteFn(b)) => {
                if a.args != b.args || a.depth != b.depth || a.codes() != b.codes() || a.consts().len() != b.consts().len() { return Ok(false); }
                ensure!(a.consts().len() <= 100_000 - work, "Contextual equality edge limit exceeded");
                children.try_reserve_exact(a.consts().len())?;
                children.extend(a.consts().iter().copied().zip(b.consts().iter().copied()));
            },
            // Hash/char tables and opaque handles compare native identity in the
            // existing representation, not recursively stored contents. Strings
            // compare text, ignoring properties as ordinary equal does.
            _ => { if left != right { return Ok(false); } continue; },
        }
        let pair = (left.into_raw().addr(), right.into_raw().addr());
        if completed.contains(&pair) { continue; }
        ensure!(active.insert(pair), "Distinct cyclic graph equality is unsupported");
        ensure!(pending.len().checked_add(children.len()+1).is_some_and(|n| n <= 100_000), "Contextual equality task limit exceeded");
        pending.try_reserve(children.len()+1)?;
        pending.push(Task::Leave(pair));
        for (a,b) in children.into_iter().rev() { pending.push(Task::Pair(a,b,depth+1)); }
    }
    Ok(true)
}

/// Rust hash keys currently have no Context-aware comparator. Reject Marker
/// leaves (including structural key children) before any lookup/publication;
/// never silently substitute marker identity for GNU positional equal.
pub(crate) fn reject_marker_hash_key(root: Object<'_>) -> Result<()> {
    enum Visit<'a> { Enter(Object<'a>, usize), Leave(usize) }
    let mut pending = vec![Visit::Enter(root,0)];
    let mut active = HashSet::new();
    let mut done = HashSet::new();
    let mut work = 0usize;
    while let Some(task) = pending.pop() {
        let (value, depth) = match task {
            Visit::Leave(id) => { active.remove(&id); done.insert(id); continue; },
            Visit::Enter(value,depth) => (value,depth),
        };
        work += 1;
        ensure!(work <= 100_000 && depth <= 512, "Hash key equality traversal limit exceeded");
        let mut children = Vec::new();
        match value.untag() {
            ObjectType::Marker(_) => anyhow::bail!("Marker-containing hash keys require contextual hashing, which is unsupported"),
            ObjectType::Cons(pair) => children.extend([pair.car(), pair.cdr()]),
            ObjectType::Vec(vector) => {
                ensure!(vector.len() <= 100_000-work, "Hash key edge limit exceeded");
                children.try_reserve_exact(vector.len())?;
                children.extend(vector.iter().map(|cell| cell.get()));
            },
            ObjectType::Record(record) => {
                ensure!(record.len() <= 100_000-work, "Hash key edge limit exceeded");
                children.try_reserve_exact(record.len())?;
                children.extend(record.iter().map(|cell| cell.get()));
            },
            ObjectType::ByteFn(function) => {
                ensure!(function.consts().len() <= 100_000-work, "Hash key edge limit exceeded");
                children.try_reserve_exact(function.consts().len())?;
                children.extend_from_slice(function.consts());
            },
            // Opaque hash/char tables compare identity; string properties are
            // ignored by ordinary equal. Their contents are not key edges.
            _ => continue,
        }
        let id = value.into_raw().addr();
        if done.contains(&id) { continue; }
        ensure!(active.insert(id), "Cyclic structural hash keys are unsupported");
        ensure!(pending.len().checked_add(children.len()+1).is_some_and(|n|n<=100_000), "Hash key task limit exceeded");
        pending.try_reserve(children.len()+1)?;
        pending.push(Visit::Leave(id));
        for child in children.into_iter().rev() { pending.push(Visit::Enter(child,depth+1)); }
    }
    Ok(())
}

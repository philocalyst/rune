//! Conservative guard for the existing recursive global function clone path.
//! This is validation, not a cycle-preserving graph copier.
use super::{Object, ObjectType};
use anyhow::{Result, ensure};

const MAX_DEPTH: usize = 256;
const MAX_WORK: usize = 100_000;
const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

enum Task<'a> {
    Enter(Object<'a>, usize),
    Leave(usize),
}

/// Caller must exclude collection/callbacks for the source graph. Publication
/// calls this while holding INTERNED_SYMBOLS, which also serializes insertion
/// into mutable global hash tables. Other global container mutations only
/// remove edges; char tables/vectors/conses published globally are read-only.
pub(crate) fn validate_function_clone(object: Object<'_>) -> Result<()> {
    let mut tasks = Vec::new();
    tasks.try_reserve(1)?;
    tasks.push(Task::Enter(object, 0));
    let mut active = Vec::new();
    let mut work = 0usize;
    let mut payload = 0usize;
    while let Some(task) = tasks.pop() {
        let (object, depth) = match task {
            Task::Leave(key) => {
                debug_assert_eq!(active.pop(), Some(key));
                continue;
            }
            Task::Enter(object, depth) => (object, depth),
        };
        work += 1;
        ensure!(work <= MAX_WORK, "Function definition exceeds clone work budget");
        ensure!(depth <= MAX_DEPTH, "Function definition exceeds clone depth limit");
        let remaining = MAX_WORK - work;
        let (children, bytes, container) = clone_children(object, remaining)?;
        payload = payload
            .checked_add(bytes)
            .ok_or_else(|| anyhow::anyhow!("Function definition exceeds clone payload budget"))?;
        ensure!(payload <= MAX_PAYLOAD, "Function definition exceeds clone payload budget");
        if !container {
            continue;
        }
        let key = object.into_raw().addr();
        ensure!(!active.contains(&key), "Cyclic function definition cannot be cloned");
        active.try_reserve(1)?;
        active.push(key);
        ensure!(
            tasks
                .len()
                .checked_add(children.len())
                .and_then(|n| n.checked_add(1))
                .is_some_and(|n| n <= MAX_WORK),
            "Function definition exceeds clone task budget"
        );
        tasks.try_reserve(children.len() + 1)?;
        tasks.push(Task::Leave(key));
        for child in children.into_iter().rev() {
            tasks.push(Task::Enter(child, depth + 1));
        }
    }
    Ok(())
}

fn clone_children(object: Object<'_>, limit: usize) -> Result<(Vec<Object<'_>>, usize, bool)> {
    let mut children = Vec::new();
    let (bytes, container) = match object.untag() {
        ObjectType::Cons(pair) => {
            ensure!(limit >= 2, "Function definition exceeds clone work budget");
            children.try_reserve_exact(2)?;
            children.extend([pair.car(), pair.cdr()]);
            (2 * std::mem::size_of::<Object>(), true)
        }
        ObjectType::Vec(vector) => {
            ensure!(vector.len() <= limit, "Function definition exceeds clone work budget");
            children.try_reserve_exact(vector.len())?;
            children.extend(vector.iter().map(|cell| cell.get()));
            (vector.len().saturating_mul(std::mem::size_of::<Object>()), true)
        }
        ObjectType::Record(record) => {
            ensure!(record.len() <= limit, "Function definition exceeds clone work budget");
            children.try_reserve_exact(record.len())?;
            children.extend(record.iter().map(|cell| cell.get()));
            (record.len().saturating_mul(std::mem::size_of::<Object>()), true)
        }
        ObjectType::ByteFn(function) => {
            ensure!(
                function.consts().len() <= limit,
                "Function definition exceeds clone work budget"
            );
            children.try_reserve_exact(function.consts().len())?;
            children.extend_from_slice(function.consts());
            // CloneIn also copies the constants vector, one native recursive
            // layer per ByteFn in addition to the explicit graph depth here.
            (
                function
                    .op_codes
                    .len()
                    .saturating_add(children.len().saturating_mul(std::mem::size_of::<Object>())),
                true,
            )
        }
        ObjectType::HashTable(table) => {
            // One source lock covers count and copy. Separate indexed reads
            // could miss a surviving edge after concurrent shift_remove.
            children = table.clone_children_snapshot(limit)?;
            (children.len().saturating_mul(32), true)
        }
        ObjectType::CharTable(table) => {
            children = table.clone_children_snapshot(limit)?;
            (children.len().saturating_mul(64), true)
        }
        ObjectType::String(value) => (value.len(), false),
        ObjectType::ByteString(value) => (value.len(), false),
        ObjectType::BigInt(value) => (usize::try_from(value.bits().div_ceil(8))?, false),
        // Symbols clone only their name/global function pointer, not the
        // function graph behind it. Alias cycles are a separate resolver check.
        ObjectType::Symbol(value) => (value.name().len(), false),
        ObjectType::Float(_) => (8, false),
        ObjectType::Int(_) | ObjectType::SubrFn(_) => (0, false),
        ObjectType::Overlay(_) => anyhow::bail!("Overlay cannot be published in a global function"),
        // These wrappers contain native handles, not recursively cloned Lisp
        // fields. Process callbacks belong to the Context registry, not here.
        ObjectType::Buffer(_)
        | ObjectType::ChannelSender(_)
        | ObjectType::ChannelReceiver(_)
        | ObjectType::Process(_)
        | ObjectType::Frame(_) => (64, false),
    };
    Ok((children, bytes, container))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        cons::Cons,
        env::{intern, sym},
        gc::{Context, RootSet},
        object::{CharTableInner, IntoObject, NIL},
    };
    use rune_core::macros::root;
    #[test]
    fn cyclic_lambda_is_rejected_before_publication_and_previous_function_survives_gc() {
        let roots = RootSet::default();
        let cx = &mut Context::new(&roots);
        sym::init_symbols();
        let symbol = intern("rune-guard-preserved-function", cx);
        let previous = crate::reader::read("(lambda () 7)", cx).unwrap().0;
        crate::data::fset(symbol, previous).unwrap();
        let previous = symbol.func(cx).unwrap();
        root!(previous, cx);
        let cycle = Cons::new(sym::LAMBDA, NIL, cx);
        cycle.set_cdr(cycle.into()).unwrap();
        root!(cycle, Object::from(cycle), cx);
        assert!(crate::data::fset(symbol, cycle.bind(cx)).is_err());
        assert!(symbol.func(cx).unwrap().ptr_eq(previous.bind(cx)));
        cx.garbage_collect(true);
        let symbol = intern("rune-guard-preserved-function", cx);
        assert!(symbol.func(cx).unwrap().ptr_eq(previous.bind(cx)));
        assert!(validate_function_clone(cycle.bind(cx)).is_err());
    }
    #[test]
    fn cyclic_vectors_records_and_bytecode_constants_are_detected_without_formatting() {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        let vector = vec![NIL].into_obj(&cx.block);
        vector.untag().try_mut().unwrap()[0].set(vector.into());
        assert!(validate_function_clone(vector.into()).is_err());
        let table = crate::core::object::HashTable::default().into_obj(&cx.block);
        table.untag().insert(Object::from(0), table.into()).unwrap();
        assert!(table.untag().clone_children_snapshot(1).is_err());
        assert_eq!(table.untag().clone_children_snapshot(2).unwrap().len(), 2);
        assert!(validate_function_clone(table.into()).is_err());
        let constants = vec![NIL].into_obj(&cx.block);
        let codes = vec![135u8].into_obj(&cx.block);
        let function = crate::alloc::make_byte_code(
            0,
            codes.untag(),
            constants.untag(),
            1,
            None,
            None,
            &[],
            cx,
        )
        .unwrap();
        constants.untag().try_mut().unwrap()[0].set(function.into());
        assert!(validate_function_clone(function.into()).is_err());
        let mut fields = bumpalo::collections::Vec::with_capacity_in(1, &cx.block.objects);
        fields.push(NIL);
        let record = crate::core::object::RecordBuilder(fields).into_obj(&cx.block);
        record.untag().try_mut().unwrap()[0].set(record.into());
        assert!(validate_function_clone(record.into()).is_err());
    }
    #[test]
    fn char_table_snapshot_is_bounded_and_parent_cycles_are_detected() {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        let table = CharTableInner::new(None).into_obj(&cx.block);
        assert!(table.untag().clone_children_snapshot(0).is_err());
        assert_eq!(table.untag().clone_children_snapshot(1).unwrap().len(), 1);
        table.untag().set(7, Object::from(3)).unwrap();
        assert!(table.untag().clone_children_snapshot(1).is_err());
        table.untag().set_parent(Some(table.untag())).unwrap();
        assert!(validate_function_clone(table.into()).is_err());
        table.untag().set_parent(None).unwrap();
        assert!(validate_function_clone(table.into()).is_ok());
    }
    #[test]
    fn repeated_dag_expansion_and_deep_acyclic_graphs_have_bounded_rejection() {
        let roots = RootSet::default();
        let cx = &Context::new(&roots);
        let mut dag = NIL;
        for _ in 0..20 {
            dag = cx.add(vec![dag, dag]);
        }
        assert!(validate_function_clone(dag).is_err());
        let mut deep = NIL;
        for _ in 0..MAX_DEPTH + 2 {
            deep = Cons::new(deep, NIL, cx).into();
        }
        assert!(validate_function_clone(deep).is_err());
        assert!(validate_function_clone(cx.add("ordinary payload")).is_ok());
        let large = cx.add("x".repeat(256 * 1024));
        assert!(validate_function_clone(cx.add(vec![large; 100])).is_err());
    }
}

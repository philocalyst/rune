//! Owner-Env variable links. Values and links remain distinct traced slots.
//! Resolution never evaluates Lisp or permits collection while native identity
//! keys are live. Binding decisions must use the original name before resolving.
use super::*;
use std::collections::HashSet;

impl RootedEnv<'_> {
    pub(crate) fn indirect_variable<'cx>(
        &self,
        variable: Symbol,
        cx: &'cx Context,
    ) -> Result<Symbol<'cx>> {
        let mut current = cx.bind(variable);
        let mut seen = HashSet::new();
        for _ in 0..100_000 {
            seen.try_reserve(1)?;
            // Symbol equality/hash are identity based, including uninterned names.
            anyhow::ensure!(seen.insert(current), "Cyclic variable indirection");
            match self.variable_aliases.get(current) {
                Some(next) => current = next.bind(cx),
                None => return Ok(current),
            }
        }
        Err(anyhow!("Variable indirection exceeds traversal budget"))
    }
}

impl RootedEnv<'_> {
    /// Install an immediate edge only after validating its entire target chain.
    /// Fresh dynamic source bindings cannot be redirected; existing aliases can
    /// be retargeted because their unwind records already capture canonical bases.
    pub(crate) fn define_variable_alias(
        &mut self,
        alias: Symbol,
        base: Symbol,
        doc: Object,
        cx: &Context,
    ) -> Result<()> {
        use crate::{core::cons::Cons, eval::EvalError};
        let reject_cycle = || -> anyhow::Error {
            EvalError::signal(
                sym::CYCLIC_VARIABLE_INDIRECTION.into(),
                Cons::new1(base, cx).into(),
                cx,
            )
            .into()
        };
        let reject = |message: String| -> anyhow::Error {
            let message = cx.add(message);
            EvalError::signal(sym::ERROR.into(), Cons::new1(message, cx).into(), cx).into()
        };
        if alias.is_const() || alias.is_builtin_value_constant() {
            return Err(reject(format!("Cannot make a constant an alias: {alias}")));
        }
        if [
            sym::BUFFER_READ_ONLY,
            sym::DEFAULT_DIRECTORY,
            sym::FILL_COLUMN,
            sym::LOAD_PATH,
            sym::CURRENT_TIME_LIST,
            sym::CASE_FOLD_SEARCH,
        ]
        .contains(&alias)
        {
            let message = if alias == sym::CASE_FOLD_SEARCH {
                format!("Don’t know how to make a buffer-local variable an alias: {alias}")
            } else {
                format!("Cannot make a built-in variable an alias: {alias}")
            };
            return Err(reject(message));
        }
        let existing = self.variable_aliases.get(alias).is_some();
        if !existing && self.binding_stack.bind_ref(cx).iter().any(|(name, _)| **name == alias) {
            return Err(reject(format!(
                "Don’t know how to make a let-bound variable an alias: {alias}"
            )));
        }
        let mut cursor = cx.bind(base);
        let mut seen = HashSet::new();
        for _ in 0..100_000 {
            if cursor == alias {
                return Err(reject_cycle());
            }
            seen.try_reserve(1)?;
            if !seen.insert(cursor) {
                return Err(reject_cycle());
            }
            match self.variable_aliases.get(cursor) {
                Some(next) => cursor = next.bind(cx),
                None => break,
            }
        }
        anyhow::ensure!(
            self.variable_aliases.get(cursor).is_none(),
            "Variable alias traversal budget exceeded"
        );
        let old = self.variable_value(alias, cx)?;
        let target = self.variable_value(base, cx)?;
        // All fallible capacity preparation precedes any value/edge publication.
        self.variable_aliases.try_reserve(1)?;
        self.vars.try_reserve(1)?;
        self.props.try_reserve(1)?;
        let mut fresh_plist = Vec::new();
        if let Some(plist) = self.props.get_mut(alias) {
            plist.bind_mut(cx).try_reserve(1)?;
        } else {
            fresh_plist.try_reserve_exact(1)?;
            fresh_plist.push((sym::VARIABLE_DOCUMENTATION, doc));
        }
        if target.is_none() {
            if let Some(value) = old {
                self.set_var_context(cursor, value, cx)?;
            }
        }
        self.variable_aliases.insert(alias, base);
        self.vars.remove(alias);
        alias.make_special();
        base.make_special();
        if fresh_plist.is_empty() {
            self.set_prop(alias, sym::VARIABLE_DOCUMENTATION, doc);
        } else {
            self.props.insert(alias, fresh_plist);
        }
        Ok(())
    }
}

impl RootedEnv<'_> {
    pub(crate) fn variable_documentation_property<'cx>(
        &self,
        symbol: Symbol,
        property: Symbol,
        cx: &'cx Context,
    ) -> Result<Object<'cx>> {
        let mut cursor = cx.bind(symbol);
        let mut seen = HashSet::new();
        for _ in 0..100_000 {
            seen.try_reserve(1)?;
            anyhow::ensure!(seen.insert(cursor), "Cyclic variable documentation indirection");
            let value = self
                .props
                .get(cursor)
                .and_then(|plist| plist.iter().find(|entry| entry.0 == property))
                .map(|entry| entry.1.bind(cx))
                .unwrap_or(crate::core::object::NIL);
            if !value.is_nil() || property != sym::VARIABLE_DOCUMENTATION {
                return Ok(value);
            }
            match self.variable_aliases.get(cursor) {
                Some(next) => cursor = next.bind(cx),
                None => return Ok(value),
            }
        }
        Err(anyhow!("Variable documentation exceeds traversal budget"))
    }
}

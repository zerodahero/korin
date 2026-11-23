//! Skipping stages: which other stages a skip takes with it, and why.
//!
//! A skip leaves unset every value the skipped steps would have made. A later stage that reads
//! one of those values, outside an `exists` test, cannot run either: it is a dependent. So is a
//! stage that a skipped stage names in `skip also`. Each new dependent leaves its own values
//! unset in turn, so the closure repeats until nothing changes. A value someone gives by hand
//! keeps the stages that read it.

use std::collections::BTreeMap;

use crate::ir::{Program, SkipRule, Sym};

/// Why a stage does not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The operator skipped it at a prompt.
    Operator,
    /// `--skip`.
    Cli,
    /// `--skip-preflight`.
    SkipPreflight,
    /// It comes before the `--from` stage, so it ran in an earlier run. Its skip rules do not
    /// apply.
    From,
    /// A skipped stage names it in `skip also`.
    Also(usize),
    /// It reads values a skip leaves unset.
    Reads(Vec<Sym>),
}

impl Why {
    /// Who or what skipped it, as the journal's `stage_skip` event names it.
    pub fn by(&self) -> &'static str {
        match self {
            Why::Operator => "operator",
            Why::Cli => "--skip",
            Why::SkipPreflight => "--skip-preflight",
            Why::From => "--from",
            Why::Also(_) => "skip also",
            Why::Reads(_) => "dependency",
        }
    }

    /// Whether the stage's own skip rules apply. A stage before `--from` is not skipped in
    /// this run; it is taken as done.
    pub fn rules_apply(&self) -> bool {
        !matches!(self, Why::From)
    }

    /// Why, in words, for the line under a skipped stage's rule.
    pub fn describe(&self, program: &Program) -> String {
        match self {
            Why::Operator => "skipped".into(),
            Why::Cli => "skipped by --skip".into(),
            Why::SkipPreflight => "skipped by --skip-preflight".into(),
            Why::From => "before --from".into(),
            Why::Also(from) => format!("skipped: `skip also` in {}", program.stage(*from).name()),
            Why::Reads(syms) => {
                let first = syms[0];
                let info = program.sym(first);
                let from = info.stage.map_or_else(String::new, |s| {
                    format!(" from {}", program.stage(s).name())
                });
                let more = match syms.len() {
                    1 => String::new(),
                    n => format!(" and {} more", n - 1),
                };
                format!("skipped: reads `{}`{from}{more}", info.name)
            }
        }
    }
}

/// What a set of skips amounts to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Closure {
    /// Every stage that will not run, by number (0 is the preflight), with why.
    pub stages: BTreeMap<usize, Why>,
    /// The values that dependents read and nobody gave, in definition order. Giving all of
    /// them keeps every dependent that only reads.
    pub missing: Vec<Sym>,
    /// Dependents that are `skip never`: the skip may go ahead only once their values are
    /// given.
    pub blocked: Vec<usize>,
}

impl Closure {
    /// The stages that will not run because of others: dependents and `skip also` targets.
    pub fn dependents(&self) -> impl Iterator<Item = (usize, &Why)> {
        self.stages
            .iter()
            .filter(|(_, why)| matches!(why, Why::Also(_) | Why::Reads(_)))
            .map(|(n, why)| (*n, why))
    }

    /// The dependents that read values, as opposed to `skip also` targets.
    pub fn readers(&self) -> impl Iterator<Item = (usize, &[Sym])> {
        self.stages.iter().filter_map(|(n, why)| match why {
            Why::Reads(syms) => Some((*n, syms.as_slice())),
            _ => None,
        })
    }
}

/// Works out what skipping `decided` (and, with `partial`, the values the rest of the
/// current stage would make) takes with it. Only stages numbered above `after` can become
/// dependents: the ones up to it have run, or are running. `have` says whether a value is
/// given anyway, by hand or on the command line.
pub fn closure(
    program: &Program,
    decided: &BTreeMap<usize, Why>,
    partial: &[Sym],
    after: Option<usize>,
    have: &dyn Fn(Sym) -> bool,
) -> Closure {
    let mut stages = decided.clone();
    let candidate = |n: usize| after.is_none_or(|a| n > a);
    loop {
        let mut changed = false;
        let unset = |sym: Sym, stages: &BTreeMap<usize, Why>| {
            let made_by_skip = program
                .sym(sym)
                .stage
                .is_some_and(|s| stages.contains_key(&s))
                || partial.contains(&sym);
            made_by_skip && !have(sym)
        };
        // `skip also` of every skipped stage whose rules apply.
        let also: Vec<(usize, usize)> = stages
            .iter()
            .filter(|(_, why)| why.rules_apply())
            .flat_map(|(n, _)| match &program.stage(*n).skip {
                SkipRule::Also(targets) => targets.iter().map(|t| (*n, *t)).collect(),
                _ => Vec::new(),
            })
            .collect();
        for (from, target) in also {
            if candidate(target) && !stages.contains_key(&target) {
                stages.insert(target, Why::Also(from));
                changed = true;
            }
        }
        // Stages that read a value left unset.
        for stage in program.all_stages() {
            let n = stage.number;
            if !candidate(n) || stages.contains_key(&n) {
                continue;
            }
            let mut reads: Vec<Sym> = Vec::new();
            for r in &program.references {
                if r.stage == n && !r.guarded && unset(r.sym, &stages) && !reads.contains(&r.sym) {
                    reads.push(r.sym);
                }
            }
            if !reads.is_empty() {
                reads.sort_unstable();
                stages.insert(n, Why::Reads(reads));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut missing: Vec<Sym> = stages
        .values()
        .filter_map(|why| match why {
            Why::Reads(syms) => Some(syms.iter().copied()),
            _ => None,
        })
        .flatten()
        .collect();
    missing.sort_unstable();
    missing.dedup();
    let blocked = stages
        .iter()
        .filter(|(n, why)| {
            matches!(why, Why::Reads(_)) && program.stage(**n).skip == SkipRule::Never
        })
        .map(|(n, _)| *n)
        .collect();
    Closure {
        stages,
        missing,
        blocked,
    }
}

/// The values the steps make: captures, expects and `ask` answers, including those inside
/// blocks and templates. Loop variables are left out; they never outlive their loop.
pub fn made_by<'a>(steps: impl IntoIterator<Item = &'a crate::ir::Step>) -> Vec<Sym> {
    use crate::ir::StepKind;
    let mut out = Vec::new();
    for step in steps {
        match &step.kind {
            StepKind::Command(c) => {
                out.extend(c.captures.iter().map(|c| c.sym));
                out.extend(c.expects.iter().map(|e| e.sym));
            }
            StepKind::Ask { sym, .. } => out.push(*sym),
            StepKind::When { body, .. }
            | StepKind::Each { body, .. }
            | StepKind::Poll { body, .. }
            | StepKind::Call { body, .. } => out.extend(made_by(body)),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{Diag, SourceFile};

    fn program(body: &str) -> Program {
        let file = SourceFile::new(
            "/w.korin",
            "w.korin",
            format!("korin 1\ntitle \"t\"\n{body}"),
        );
        match crate::compile::compile(&file) {
            Ok((p, _)) => p,
            Err(diags) => panic!("{:?}", diags.iter().map(Diag::short).collect::<Vec<_>>()),
        }
    }

    fn skip(p: &Program, stage: usize, have: &[&str]) -> Closure {
        let decided = BTreeMap::from([(stage, Why::Operator)]);
        let given: Vec<Sym> = have
            .iter()
            .map(|n| p.syms.iter().position(|s| s.name == *n).unwrap())
            .collect();
        closure(p, &decided, &[], Some(stage), &|s| given.contains(&s))
    }

    fn numbers(c: &Closure) -> Vec<usize> {
        c.stages.keys().copied().collect()
    }

    const CHAIN: &str = concat!(
        "stage \"a\"\n  read echo x\n    capture x = text\n",
        "stage \"b\"\n  read echo {x}\n    capture y = text\n",
        "stage \"c\"\n  read echo {y}\n",
        "stage \"d\"\n  read true\n",
    );

    #[test]
    fn a_reader_is_a_dependent_and_so_are_its_readers() {
        let p = program(CHAIN);
        let c = skip(&p, 1, &[]);
        assert_eq!(numbers(&c), [1, 2, 3]);
        let x = p.syms.iter().position(|s| s.name == "x").unwrap();
        let y = p.syms.iter().position(|s| s.name == "y").unwrap();
        assert_eq!(c.missing, [x, y]);
    }

    #[test]
    fn a_value_given_by_hand_keeps_its_readers() {
        let p = program(CHAIN);
        assert_eq!(numbers(&skip(&p, 1, &["x"])), [1]);
    }

    #[test]
    fn a_guarded_read_is_not_a_dependency() {
        let p = program(concat!(
            "stage \"a\"\n  read echo x\n    capture x? = regex /(x)/\n",
            "stage \"b\"\n  when exists x\n    read echo {x}\n",
        ));
        assert_eq!(numbers(&skip(&p, 1, &[])), [1]);
    }

    #[test]
    fn skip_also_takes_its_targets_even_with_values_given() {
        let p = program(concat!(
            "stage \"a\"\n  skip also \"c\"\n  read true\n",
            "stage \"b\"\n  read true\n",
            "stage \"c\"\n  read true\n",
        ));
        let c = skip(&p, 1, &[]);
        assert_eq!(numbers(&c), [1, 3]);
        assert_eq!(c.stages[&3], Why::Also(1));
    }

    #[test]
    fn a_skip_never_dependent_blocks_until_its_values_are_given() {
        let p = program(concat!(
            "stage \"a\"\n  read echo x\n    capture x = text\n",
            "stage \"b\"\n  skip never\n  read echo {x}\n",
        ));
        assert_eq!(skip(&p, 1, &[]).blocked, [2]);
        assert_eq!(skip(&p, 1, &["x"]).blocked, Vec::<usize>::new());
    }

    #[test]
    fn from_ignores_the_skip_rules_of_earlier_stages() {
        let p = program(concat!(
            "stage \"a\"\n  skip also \"c\"\n  read true\n",
            "stage \"b\"\n  read true\n",
            "stage \"c\"\n  read true\n",
        ));
        let decided = BTreeMap::from([(1, Why::From)]);
        let c = closure(&p, &decided, &[], None, &|_| false);
        assert_eq!(numbers(&c), [1]);
    }
}

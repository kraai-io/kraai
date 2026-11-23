use nu_engine::command_prelude::*;
use nu_protocol::ast::{
    Block, Expr, Expression, Operator, Pipeline, PipelineRedirection, RedirectionSource, Traverse,
};
use nu_protocol::ir::{Instruction, IrBlock};
use nu_protocol::{BlockId, DeclId};
use std::sync::Arc;

use super::HostError;
use super::sink::OutputSink;

const EMIT_COMMAND: &str = "__kraai_emit";

pub(super) fn register(
    working_set: &mut StateWorkingSet<'_>,
    sink: &Arc<OutputSink>,
) -> Result<(), HostError> {
    if working_set.find_decl(EMIT_COMMAND.as_bytes()).is_some() {
        return Err(HostError::Initialization(format!(
            "{EMIT_COMMAND} is already defined"
        )));
    }
    working_set.add_decl(Box::new(Emit { sink: sink.clone() }));
    Ok(())
}

pub(super) fn emit_decl(working_set: &StateWorkingSet<'_>) -> Option<DeclId> {
    working_set.find_decl(EMIT_COMMAND.as_bytes())
}

pub(super) fn emit_drained_statements(
    working_set: &StateWorkingSet<'_>,
    statement: &Block,
    ir: &mut IrBlock,
    emit: DeclId,
) {
    emit_drains(ir, emit, &value_spans(working_set, statement));
}

fn value_spans(working_set: &StateWorkingSet<'_>, block: &Block) -> Vec<Span> {
    let mut spans = Vec::new();
    block.flat_map(
        working_set,
        &|expression: &Expression| match &expression.expr {
            Expr::Call(call)
                if matches!(
                    working_set.get_decl(call.decl_id).name(),
                    "let" | "mut" | "const"
                ) =>
            {
                vec![expression.span]
            }
            Expr::BinaryOp(_, operator, _)
                if matches!(operator.expr, Expr::Operator(Operator::Assignment(_))) =>
            {
                vec![expression.span]
            }
            Expr::Subexpression(_) => vec![expression.span],
            _ => Vec::new(),
        },
        &mut spans,
    );
    spans
}

pub(super) fn do_body(working_set: &StateWorkingSet<'_>, pipeline: &Pipeline) -> Option<BlockId> {
    let [element] = pipeline.elements.as_slice() else {
        return None;
    };
    let stderr_only = matches!(
        element.redirection,
        Some(PipelineRedirection::Single {
            source: RedirectionSource::Stderr,
            ..
        })
    );
    if element.redirection.is_some() && !stderr_only {
        return None;
    }
    let Expr::Call(call) = &element.expr.expr else {
        return None;
    };
    if working_set.get_decl(call.decl_id).name() != "do" {
        return None;
    }
    match call.positional_iter().next()?.expr {
        Expr::Closure(block_id) | Expr::Block(block_id) => Some(block_id),
        _ => None,
    }
}

pub(super) fn emit_body_statements(
    working_set: &mut StateWorkingSet<'_>,
    body: BlockId,
    emit: DeclId,
) {
    let assignments = value_spans(working_set, working_set.get_block(body));
    if let Some(ir) = working_set.get_block_mut(body).ir_block.as_mut() {
        emit_drains(ir, emit, &assignments);
    }
}

fn emit_drains(ir: &mut IrBlock, emit: DeclId, assignments: &[Span]) {
    let drains: Vec<usize> = ir
        .instructions
        .iter()
        .zip(&ir.spans)
        .enumerate()
        .filter_map(|(index, (instruction, span))| {
            (matches!(instruction, Instruction::Drain { .. })
                && !assignments
                    .iter()
                    .any(|assignment| assignment.contains_span(*span)))
            .then_some(index)
        })
        .collect();
    let length = ir.instructions.len();
    if drains.is_empty() || ir.spans.len() != length || ir.comments.len() != length {
        return;
    }
    let shifted = |target: usize| target + drains.partition_point(|&drain| drain < target);
    let mut instructions = Vec::with_capacity(ir.instructions.len() + drains.len());
    let mut spans = Vec::with_capacity(instructions.capacity());
    let mut comments = Vec::with_capacity(instructions.capacity());
    let old = std::mem::take(&mut ir.instructions)
        .into_iter()
        .zip(std::mem::take(&mut ir.spans))
        .zip(std::mem::take(&mut ir.comments));
    for (index, ((mut instruction, span), comment)) in old.enumerate() {
        if let Instruction::Drain { src } = instruction
            && drains.binary_search(&index).is_ok()
        {
            instructions.push(Instruction::Call {
                decl_id: emit,
                src_dst: src,
            });
            spans.push(span);
            comments.push(Box::default());
        }
        if let Some(target) = instruction.branch_target() {
            let _ = instruction.set_branch_target(shifted(target));
        }
        instructions.push(instruction);
        spans.push(span);
        comments.push(comment);
    }
    ir.instructions = instructions;
    ir.spans = spans;
    ir.comments = comments;
    for region in &mut ir.scope_regions {
        region.start = shifted(region.start);
        region.end = shifted(region.end);
    }
}

#[derive(Clone)]
struct Emit {
    sink: Arc<OutputSink>,
}

impl Command for Emit {
    fn name(&self) -> &str {
        EMIT_COMMAND
    }

    fn signature(&self) -> Signature {
        Signature::build(EMIT_COMMAND).category(Category::Core)
    }

    fn description(&self) -> &str {
        "Internal: report a statement result."
    }

    fn run(
        &self,
        engine: &EngineState,
        stack: &mut Stack,
        _call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        super::output::emit(&self.sink, engine, stack, input).map_err(|error| *error)?;
        Ok(PipelineData::empty())
    }
}

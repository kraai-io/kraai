use nu_engine::command_prelude::*;
use nu_protocol::DeclId;
use nu_protocol::ast::{Expr, Operator, Pipeline};
use nu_protocol::ir::{Instruction, IrBlock};
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

pub(super) fn is_assignment(working_set: &StateWorkingSet<'_>, pipeline: &Pipeline) -> bool {
    let [element] = pipeline.elements.as_slice() else {
        return false;
    };
    match &element.expr.expr {
        Expr::Call(call) => matches!(
            working_set.get_decl(call.decl_id).name(),
            "let" | "mut" | "const"
        ),
        Expr::BinaryOp(_, operator, _) => {
            matches!(operator.expr, Expr::Operator(Operator::Assignment(_)))
        }
        _ => false,
    }
}

pub(super) fn emit_drained_statements(ir: &mut IrBlock, emit: DeclId) {
    let drains: Vec<usize> = ir
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            matches!(instruction, Instruction::Drain { .. }).then_some(index)
        })
        .collect();
    let length = ir.instructions.len();
    if drains.is_empty()
        || ir.spans.len() != length
        || ir.ast.len() != length
        || ir.comments.len() != length
    {
        return;
    }
    let shifted = |target: usize| target + drains.partition_point(|&drain| drain < target);
    let mut instructions = Vec::with_capacity(ir.instructions.len() + drains.len());
    let mut spans = Vec::with_capacity(instructions.capacity());
    let mut ast = Vec::with_capacity(instructions.capacity());
    let mut comments = Vec::with_capacity(instructions.capacity());
    let old = std::mem::take(&mut ir.instructions)
        .into_iter()
        .zip(std::mem::take(&mut ir.spans))
        .zip(std::mem::take(&mut ir.ast))
        .zip(std::mem::take(&mut ir.comments));
    for (((mut instruction, span), ast_ref), comment) in old {
        if let Instruction::Drain { src } = instruction {
            instructions.push(Instruction::Call {
                decl_id: emit,
                src_dst: src,
            });
            spans.push(span);
            ast.push(ast_ref.clone());
            comments.push(Box::default());
        }
        if let Some(target) = instruction.branch_target() {
            let _ = instruction.set_branch_target(shifted(target));
        }
        instructions.push(instruction);
        spans.push(span);
        ast.push(ast_ref);
        comments.push(comment);
    }
    ir.instructions = instructions;
    ir.spans = spans;
    ir.ast = ast;
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

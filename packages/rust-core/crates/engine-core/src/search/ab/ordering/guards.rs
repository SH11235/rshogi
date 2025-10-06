use crate::evaluation::evaluate::Evaluator;
use crate::Position;

#[must_use]
pub(crate) struct EvalMoveGuard<'a, T: Evaluator + ?Sized> {
    evaluator: &'a T,
}

impl<'a, T: Evaluator + ?Sized> EvalMoveGuard<'a, T> {
    pub(crate) fn new(evaluator: &'a T, pos: &Position, mv: crate::shogi::Move) -> Self {
        evaluator.on_do_move(pos, mv);
        Self { evaluator }
    }
}

impl<'a, T: Evaluator + ?Sized> Drop for EvalMoveGuard<'a, T> {
    fn drop(&mut self) {
        self.evaluator.on_undo_move();
    }
}

#[must_use]
pub(crate) struct EvalNullGuard<'a, T: Evaluator + ?Sized> {
    evaluator: &'a T,
}

impl<'a, T: Evaluator + ?Sized> EvalNullGuard<'a, T> {
    pub(crate) fn new(evaluator: &'a T, pos: &Position) -> Self {
        evaluator.on_do_null_move(pos);
        Self { evaluator }
    }
}

impl<'a, T: Evaluator + ?Sized> Drop for EvalNullGuard<'a, T> {
    fn drop(&mut self) {
        self.evaluator.on_undo_null_move();
    }
}

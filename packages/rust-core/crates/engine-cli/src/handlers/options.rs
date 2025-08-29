use crate::command_handler::CommandContext;
use crate::worker::lock_or_recover_adapter;

pub(crate) fn handle_set_option(
    name: String,
    value: Option<String>,
    ctx: &mut CommandContext,
) -> anyhow::Result<()> {
    let mut engine = lock_or_recover_adapter(ctx.engine);
    engine.set_option(&name, value.as_deref())?;
    Ok(())
}

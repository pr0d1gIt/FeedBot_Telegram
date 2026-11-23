//! Telegram bot commands.
//!
//! Uses `teloxide`'s `BotCommands` derive to declare each supported command.

use teloxide::utils::command::BotCommands;

/// Commands accepted by the bot.
#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "lowercase",
    description = "Comandos disponibles del bot de ciberseguridad:"
)]
pub enum Command {
    #[command(description = "Mostrar el mensaje de bienvenida y la lista de comandos.")]
    Start,

    #[command(description = "Igual que /start — muestra esta ayuda.")]
    Help,

    #[command(description = "Obtener un resumen de noticias bajo demanda.")]
    Latest,

    #[command(description = "Activar notificaciones automáticas cada 12 horas.")]
    Subscribe,

    #[command(description = "Desactivar notificaciones automáticas.")]
    Unsubscribe,

    #[command(description = "Mostrar el estado de la suscripción.")]
    Status,
}
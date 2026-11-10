//! Codex's app-server pipe protocol: bounded argv and explicit speed on new and resumed chats.
use super::Made;
#[path = "codex_appserver.rs"]
mod appserver;

pub const SPEED_KEY: &str = "codex.speed";
pub const SPEEDS: &[(&str, &str)] = &[("", "Codex default"), ("standard", "Standard"), ("fast", "Fast / priority")];
pub fn service_tier(speed: &str) -> Option<&'static str> { match speed { "standard" => Some("default"), "fast" => Some("fast"), _ => None } }

pub fn driver(provider: &str) -> Option<Made> {
    if provider != "codex" { return None }
    Some(Made { cli: "codex", driver: Box::new(appserver::AppServer::default()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn codex_speed_uses_the_pipe_driver_on_every_platform() {
        let d = driver("codex").unwrap();
        let a = super::super::TurnIn { prompt: "p".repeat(100000), text: "t".repeat(100000), ..Default::default() };
        assert_eq!(d.driver.argv(&a)[0], "app-server");
        assert!(d.driver.argv(&a).join(" ").len() < 1024);
        assert!(driver("other").is_none());
    }
}

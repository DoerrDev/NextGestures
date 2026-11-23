use anyhow::{Result, bail};
use gesture_core::{Action, PluginSettings};
use tauri::AppHandle;

pub fn execute(app: &AppHandle, action: &Action, plugins: &PluginSettings) -> Result<()> {
    match action {
        Action::Screenshot if plugins.screenshot => crate::pin::spawn(app.clone()),
        Action::RecordRegion if plugins.recorder => crate::recorder::spawn(app.clone()),
        Action::AiJyutping | Action::AiTranslate | Action::AiPolish if plugins.quickformater => {
            return crate::quickformater::spawn(app, action, &plugins.ai);
        }
        _ => bail!("请先在设置中启用对应插件"),
    }
    Ok(())
}

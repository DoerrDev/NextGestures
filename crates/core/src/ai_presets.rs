//! AI 动作预设：首次启动时写入「我的动作」，之后与用户自建的动作完全一样，可改可删。
//! 提示词文本放在 `crates/core/presets/`。

use crate::config::{AiAction, ResultMode};

pub const TRANSLATE_ID: &str = "preset-translate";
pub const POLISH_ID: &str = "preset-polish";
pub const JYUTPING_ID: &str = "preset-jyutping";

const SELECTION_BLOCK: &str = "\n\n<text>\n{{selection}}\n</text>";
const POLISH_WITHOUT_NOTES: &str = "Output only the corrected text. Do not include notes.";

fn template(instructions: &str) -> String {
    format!("{}{SELECTION_BLOCK}", instructions.trim())
}

fn polish_prompt(notes: bool) -> String {
    let base = include_str!("../presets/polish.txt").trim();
    let tail = if notes { include_str!("../presets/polish_notes.txt").trim() } else { POLISH_WITHOUT_NOTES };
    template(&format!("{base}\n{tail}"))
}

/// 全部预设；`polish_notes` 决定润色是否附带「修改说明」（沿用旧版「显示英语优化提示」开关）
pub fn ai_presets(polish_notes: bool) -> Vec<AiAction> {
    vec![
        AiAction {
            id: TRANSLATE_ID.into(),
            name: "AI 翻译".into(),
            prompt: template(include_str!("../presets/translate.txt")),
            mode: ResultMode::Bubble,
            icon: "Languages".into(),
            notes: false,
        },
        AiAction {
            id: POLISH_ID.into(),
            name: "AI 润色".into(),
            prompt: polish_prompt(polish_notes),
            mode: ResultMode::Replace,
            icon: "PenLine".into(),
            notes: polish_notes,
        },
        AiAction {
            id: JYUTPING_ID.into(),
            name: "粤拼".into(),
            prompt: template(include_str!("../presets/jyutping.txt")),
            mode: ResultMode::Bubble,
            icon: "Type".into(),
            notes: false,
        },
    ]
}

/// 旧版内置动作的 kind → 对应预设 id
pub fn legacy_kind_to_preset(kind: &str) -> Option<&'static str> {
    match kind {
        "ai_translate" => Some(TRANSLATE_ID),
        "ai_polish" => Some(POLISH_ID),
        "ai_jyutping" => Some(JYUTPING_ID),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_valid_templates() {
        for notes in [true, false] {
            for preset in ai_presets(notes) {
                assert_eq!(preset.prompt.matches("{{selection}}").count(), 1, "{}", preset.id);
            }
        }
        assert!(polish_prompt(true).contains("<notes>"));
        assert!(polish_prompt(false).contains("Do not include notes"));
    }
}

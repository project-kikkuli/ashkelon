use std::path::PathBuf;

use anyhow::Context;

use super::{build_home_overlay, LaunchOptions, LaunchPlan, OverlayHome};

pub fn plan(relay_base: &str, launch: &str, args: &[String], options: &LaunchOptions) -> anyhow::Result<LaunchPlan> {
    let real_home = std::env::var_os("CURSOR_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(|path| PathBuf::from(path).join("cursor")))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".cursor"));
    let overlay_dir = options.state_dir.join("launch").join(format!("{launch}-cursor"));
    let name = "cli-config.json";
    let config = real_home.join(name);
    let mut document: serde_json::Value = if config.exists() {
        serde_json::from_slice(&std::fs::read(&config)?).context("reading Cursor CLI settings")?
    } else {
        serde_json::json!({})
    };
    document["network"]["useHttp1ForAgent"] = true.into();
    build_home_overlay(&real_home, &overlay_dir, name)?;
    crate::fsperm::write_private_file(&overlay_dir.join(name), &serde_json::to_vec_pretty(&document)?)?;
    let endpoint = format!("{}/cursor", relay_base.replacen("127.0.0.1", "localhost", 1));
    let mut plan = LaunchPlan::new("cursor-agent", "cursor");
    plan.args = args.to_vec();
    plan.env = vec![
        ("CURSOR_CONFIG_DIR".into(), overlay_dir.to_string_lossy().into_owned()),
        ("CURSOR_API_ENDPOINT".into(), endpoint.clone()),
        ("CURSOR_API_BASE_URL".into(), endpoint),
    ];
    plan.overlay_home = Some(OverlayHome {
        overlay_dir,
        real_home,
        generated_file_name: name.into(),
    });
    Ok(plan)
}

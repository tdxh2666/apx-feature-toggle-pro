#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result};
use chrono::Local;
use eframe::egui;
use egui::{Color32, FontData, FontDefinitions, FontFamily, Layout, RichText, ScrollArea};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use winreg::enums::*;
use winreg::RegKey;

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

// ============================================================
// Feature model
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FeatureEntry {
    id: String,
    description: String,

    #[serde(skip)]
    selected: bool,

    target_enabled_state: u32,
    target_enabled_options: u32,
}

impl FeatureEntry {
    fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            selected: false,
            target_enabled_state: 2,
            target_enabled_options: 0,
        }
    }

    fn reg_path(&self) -> String {
        format!(
            "SYSTEM\\CurrentControlSet\\Control\\FeatureManagement\\Overrides\\8\\{}",
            self.id
        )
    }
}

fn default_features() -> Vec<FeatureEntry> {
    vec![
        FeatureEntry::new("56246057", "Intel APX (Advanced Performance Extensions)"),
        FeatureEntry::new("58989092", "Low Latency Profile (K2 CPU Boost)"),
        FeatureEntry::new("60716524", "LLP Related Feature"),
        FeatureEntry::new("48433719", "LLP Related Feature"),
        FeatureEntry::new("61391826", "LLP Related Feature"),
    ]
}

// ============================================================
// Registry state
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq)]
enum FeatureState {
    Unknown,
    Disabled,
    Enabled,
    Partial,
}

impl FeatureState {
    fn label(&self) -> &'static str {
        match self {
            FeatureState::Unknown => "未知",
            FeatureState::Disabled => "已禁用",
            FeatureState::Enabled => "已启用",
            FeatureState::Partial => "部分",
        }
    }

    fn color(&self) -> Color32 {
        match self {
            FeatureState::Unknown => Color32::GRAY,
            FeatureState::Disabled => Color32::from_rgb(220, 90, 90),
            FeatureState::Enabled => Color32::from_rgb(90, 220, 130),
            FeatureState::Partial => Color32::from_rgb(240, 190, 90),
        }
    }
}

fn read_state(feature: &FeatureEntry) -> FeatureState {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    match hklm.open_subkey(feature.reg_path()) {
        Ok(key) => {
            let state = key.get_value::<u32, _>("EnabledState");
            let options = key.get_value::<u32, _>("EnabledStateOptions");

            match (state, options) {
                (Ok(2), Ok(0)) => FeatureState::Enabled,
                (Ok(0), _) => FeatureState::Disabled,
                (Ok(_), _) => FeatureState::Partial,
                _ => FeatureState::Unknown,
            }
        }
        Err(_) => FeatureState::Unknown,
    }
}

fn enable_feature(feature: &FeatureEntry) -> Result<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    let (key, _) = hklm
        .create_subkey(feature.reg_path())
        .context("创建注册表键失败，请确认已以管理员身份运行")?;

    key.set_value("EnabledState", &feature.target_enabled_state)
        .context("写入 EnabledState 失败")?;

    key.set_value("EnabledStateOptions", &feature.target_enabled_options)
        .context("写入 EnabledStateOptions 失败")?;

    Ok(())
}

fn disable_feature(feature: &FeatureEntry) -> Result<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    match hklm.delete_subkey_all(feature.reg_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::anyhow!("删除注册表键失败: {}", e)),
    }
}

fn export_reg_text(feature: &FeatureEntry) -> Result<String> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let key = hklm
        .open_subkey(feature.reg_path())
        .context("注册表键不存在")?;

    let mut lines = vec![format!(
        "[HKEY_LOCAL_MACHINE\\{}]",
        feature.reg_path()
    )];

    if let Ok(v) = key.get_value::<u32, _>("EnabledState") {
        lines.push(format!("\"EnabledState\"=dword:{:08x}", v));
    }

    if let Ok(v) = key.get_value::<u32, _>("EnabledStateOptions") {
        lines.push(format!("\"EnabledStateOptions\"=dword:{:08x}", v));
    }

    if lines.len() == 1 {
        return Err(anyhow::anyhow("没有可备份的值"));
    }

    Ok(lines.join("\r\n"))
}

// ============================================================
// Backup
// ============================================================

fn backup_dir() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let dir = exe_dir.join("backups");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn auto_backup(features: &[FeatureEntry]) -> Result<PathBuf> {
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let backup_path = backup_dir().join(format!("backup_{}.reg", timestamp));

    let mut content = String::from("Windows Registry Editor Version 5.00\r\n\r\n");
    let mut has_content = false;

    for feature in features {
        if let Ok(text) = export_reg_text(feature) {
            content.push_str(&text);
            content.push_str("\r\n\r\n");
            has_content = true;
        }
    }

    if !has_content {
        content.push_str("; No existing keys to backup\r\n");
    }

    fs::write(&backup_path, content).context("写入备份文件失败")?;
    Ok(backup_path)
}

fn list_backups() -> Vec<PathBuf> {
    let dir = backup_dir();
    let mut backups = Vec::new();

    if let Ok(rd) = fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "reg") {
                backups.push(path);
            }
        }
    }

    backups.sort();
    backups.reverse();
    backups
}

fn restore_backup(path: &Path) -> Result<()> {
    std::process::Command::new("regedit")
        .args(["/s", path.to_string_lossy().as_ref()])
        .spawn()
        .context("启动 regedit 失败")?;

    Ok(())
}

// ============================================================
// Config import/export
// ============================================================

#[derive(Debug, Serialize, Deserialize)]
struct AppConfig {
    version: String,
    features: Vec<FeatureEntry>,
}

fn export_config(features: &[FeatureEntry], path: &Path) -> Result<()> {
    let config = AppConfig {
        version: env!("CARGO_PKG_VERSION").to_string(),
        features: features.to_vec(),
    };

    let json = serde_json::to_string_pretty(&config).context("序列化配置失败")?;
    fs::write(path, json).context("写入配置文件失败")?;
    Ok(())
}

fn import_config(path: &Path) -> Result<Vec<FeatureEntry>> {
    let content = fs::read_to_string(path).context("读取配置文件失败")?;
    let config: AppConfig = serde_json::from_str(&content).context("解析配置文件失败")?;
    Ok(config.features)
}

// ============================================================
// Admin check
// ============================================================

fn is_admin() -> bool {
    #[cfg(windows)]
    unsafe {
        let mut token_handle = HANDLE::default();

        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle).is_err() {
            return false;
        }

        let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
        let mut return_length = 0u32;

        let ok = GetTokenInformation(
            token_handle,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut return_length,
        )
        .is_ok();

        let _ = CloseHandle(token_handle);

        ok && elevation.TokenIsElevated != 0
    }

    #[cfg(not(windows))]
    {
        false
    }
}

// ============================================================
// Logging
// ============================================================

#[derive(Debug, Clone)]
struct LogEntry {
    timestamp: String,
    message: String,
    level: LogLevel,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl LogLevel {
    fn color(&self) -> Color32 {
        match self {
            LogLevel::Info => Color32::LIGHT_GRAY,
            LogLevel::Success => Color32::from_rgb(100, 220, 140),
            LogLevel::Warning => Color32::from_rgb(240, 200, 100),
            LogLevel::Error => Color32::from_rgb(240, 100, 100),
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            LogLevel::Info => "ℹ",
            LogLevel::Success => "✔",
            LogLevel::Warning => "⚠",
            LogLevel::Error => "✘",
        }
    }
}

// ============================================================
// App
// ============================================================

struct App {
    features: Vec<FeatureEntry>,
    states: Vec<FeatureState>,
    logs: Vec<LogEntry>,
    show_restart_dialog: bool,
    new_id: String,
    new_desc: String,
    backups: Vec<PathBuf>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);

        let mut app = Self {
            features: default_features(),
            states: Vec::new(),
            logs: Vec::new(),
            show_restart_dialog: false,
            new_id: String::new(),
            new_desc: String::new(),
            backups: list_backups(),
        };

        app.log("APX Feature Toggle Pro 已启动", LogLevel::Info);
        app.log(
            &format!("管理员权限: {}", if is_admin() { "是" } else { "否" }),
            if is_admin() { LogLevel::Success } else { LogLevel::Warning },
        );

        app.sync_states();
        app
    }

    fn log(&mut self, message: &str, level: LogLevel) {
        self.logs.push(LogEntry {
            timestamp: Local::now().format("%H:%M:%S").to_string(),
            message: message.to_string(),
            level,
        });
    }

    fn sync_states(&mut self) {
        self.states = self.features.iter().map(read_state).collect();
    }

    fn selected_indices(&self) -> Vec<usize> {
        self.features
            .iter()
            .enumerate()
            .filter(|(_, f)| f.selected)
            .map(|(i, _)| i)
            .collect()
    }

    fn batch_enable(&mut self) {
        let selected = self.selected_indices();

        if selected.is_empty() {
            self.log("未选择任何 Feature", LogLevel::Warning);
            return;
        }

        let to_backup: Vec<FeatureEntry> =
            selected.iter().map(|&i| self.features[i].clone()).collect();

        match auto_backup(&to_backup) {
            Ok(path) => {
                self.log(
                    &format!("自动备份已创建: {}", path.display()),
                    LogLevel::Success,
                );
                self.backups = list_backups();
            }
            Err(e) => {
                self.log(&format!("自动备份失败: {}", e), LogLevel::Warning);
            }
        }

        let mut changed = false;

        for &idx in &selected {
            let feature = self.features[idx].clone();

            match enable_feature(&feature) {
                Ok(()) => {
                    self.log(&format!("[{}] 已启用", feature.id), LogLevel::Success);
                    changed = true;
                }
                Err(e) => {
                    self.log(
                        &format!("[{}] 启用失败: {}", feature.id, e),
                        LogLevel::Error,
                    );
                }
            }
        }

        self.sync_states();

        if changed {
            self.show_restart_dialog = true;
        }
    }

    fn batch_disable(&mut self) {
        let selected = self.selected_indices();

        if selected.is_empty() {
            self.log("未选择任何 Feature", LogLevel::Warning);
            return;
        }

        let to_backup: Vec<FeatureEntry> =
            selected.iter().map(|&i| self.features[i].clone()).collect();

        let _ = auto_backup(&to_backup);
        self.backups = list_backups();

        let mut changed = false;

        for &idx in &selected {
            let feature = self.features[idx].clone();

            match disable_feature(&feature) {
                Ok(()) => {
                    self.log(&format!("[{}] 已禁用", feature.id), LogLevel::Success);
                    changed = true;
                }
                Err(e) => {
                    self.log(
                        &format!("[{}] 禁用失败: {}", feature.id, e),
                        LogLevel::Error,
                    );
                }
            }
        }

        self.sync_states();

        if changed {
            self.log("禁用操作完成，通常也需要重启才能完全恢复状态", LogLevel::Info);
        }
    }

    fn export_config_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON 配置", &["json"])
            .set_file_name("feature_config.json")
            .save_file()
        {
            match export_config(&self.features, &path) {
                Ok(()) => {
                    self.log(
                        &format!("配置已导出: {}", path.display()),
                        LogLevel::Success,
                    );
                }
                Err(e) => {
                    self.log(&format!("导出失败: {}", e), LogLevel::Error);
                }
            }
        }
    }

    fn import_config_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON 配置", &["json"])
            .pick_file()
        {
            match import_config(&path) {
                Ok(features) => {
                    self.log(
                        &format!("已导入 {} 个 Feature", features.len()),
                        LogLevel::Success,
                    );
                    self.features = features;
                    self.sync_states();
                }
                Err(e) => {
                    self.log(&format!("导入失败: {}", e), LogLevel::Error);
                }
            }
        }
    }

    fn add_feature(&mut self) {
        let id = self.new_id.trim().to_string();
        let desc = self.new_desc.trim().to_string();

        if id.is_empty() {
            self.log("Feature ID 不能为空", LogLevel::Warning);
            return;
        }

        if self.features.iter().any(|f| f.id == id) {
            self.log(&format!("Feature ID {} 已存在", id), LogLevel::Warning);
            return;
        }

        let desc = if desc.is_empty() {
            "自定义功能".to_string()
        } else {
            desc
        };

        self.features.push(FeatureEntry::new(id.clone(), desc));
        self.log(&format!("已添加 Feature: {}", id), LogLevel::Success);

        self.new_id.clear();
        self.new_desc.clear();

        self.sync_states();
    }

    fn remove_selected(&mut self) {
        let before = self.features.len();
        self.features.retain(|f| !f.selected);
        let removed = before - self.features.len();

        if removed > 0 {
            self.log(&format!("已从列表移除 {} 个 Feature", removed), LogLevel::Success);
            self.sync_states();
        } else {
            self.log("未选择任何 Feature", LogLevel::Warning);
        }
    }

    fn restore_backup_path(&mut self, path: PathBuf) {
        match restore_backup(&path) {
            Ok(()) => {
                self.log(
                    &format!("已调用 regedit 恢复备份: {}", path.display()),
                    LogLevel::Success,
                );
                self.log("恢复后请点击“刷新状态”确认", LogLevel::Info);
            }
            Err(e) => {
                self.log(&format!("恢复失败: {}", e), LogLevel::Error);
            }
        }
    }
}

// ============================================================
// Fonts
// ============================================================

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let font_paths = [
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\msyhbd.ttc",
        "C:\\Windows\\Fonts\\simhei.ttf",
    ];

    for path in &font_paths {
        if let Ok(data) = fs::read(path) {
            fonts
                .font_data
                .insert("chinese".to_owned(), FontData::from_owned(data));

            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, "chinese".to_owned());

            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .push("chinese".to_owned());

            break;
        }
    }

    ctx.set_fonts(fonts);
}

// ============================================================
// GUI
// ============================================================

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.heading(
                    RichText::new("🔧 APX Feature Toggle Pro")
                        .strong()
                        .size(20.0),
                );
                ui.add_space(12.0);

                if is_admin() {
                    ui.label(
                        RichText::new("🛡 管理员权限已获取")
                            .color(Color32::from_rgb(100, 220, 140)),
                    );
                } else {
                    ui.label(
                        RichText::new("⚠ 当前不是管理员")
                            .color(Color32::from_rgb(240, 100, 100)),
                    );
                }
            });

            ui.add_space(4.0);
            ui.label(
                RichText::new("  Windows FeatureManagement 隐藏功能批量管理工具")
                    .color(Color32::GRAY),
            );
            ui.add_space(8.0);
        });

        egui::SidePanel::left("feature_panel")
            .resizable(true)
            .default_width(430.0)
            .min_width(360.0)
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.add_space(8.0);

                        ui.horizontal(|ui| {
                            ui.heading("📋 Feature 列表");

                            ui.with_layout(
                                Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let all_selected =
                                        !self.features.is_empty()
                                            && self.features.iter().all(|f| f.selected);

                                    let label = if all_selected { "取消全选" } else { "全选" };

                                    if ui.button(label).clicked() {
                                        let new_state = !all_selected;
                                        for f in &mut self.features {
                                            f.selected = new_state;
                                        }
                                    }
                                },
                            );
                        });

                        ui.separator();

                        let row_count = self.features.len();

                        egui::Grid::new("feature_grid")
                            .striped(true)
                            .min_col_width(55.0)
                            .show(ui, |ui| {
                                ui.label(RichText::new("选择").strong());
                                ui.label(RichText::new("Feature ID").strong());
                                ui.label(RichText::new("描述").strong());
                                ui.label(RichText::new("状态").strong());
                                ui.end_row();

                                for i in 0..row_count {
                                    let state = self
                                        .states
                                        .get(i)
                                        .copied()
                                        .unwrap_or(FeatureState::Unknown);

                                    ui.checkbox(&mut self.features[i].selected, "");
                                    ui.label(
                                        RichText::new(self.features[i].id.as_str()).monospace(),
                                    );
                                    ui.label(self.features[i].description.as_str());
                                    ui.label(
                                        RichText::new(state.label()).color(state.color()),
                                    );
                                    ui.end_row();
                                }
                            });

                        ui.add_space(8.0);
                        ui.separator();

                        ui.heading("➕ 添加 Feature");
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            ui.label("ID:");
                            ui.add_sized(
                                [130.0, 22.0],
                                egui::TextEdit::singleline(&mut self.new_id)
                                    .hint_text("例如 56246057"),
                            );
                        });

                        ui.horizontal(|ui| {
                            ui.label("描述:");
                            ui.add_sized(
                                [220.0, 22.0],
                                egui::TextEdit::singleline(&mut self.new_desc)
                                    .hint_text("功能描述"),
                            );

                            if ui.button("添加").clicked() {
                                self.add_feature();
                            }
                        });

                        ui.add_space(8.0);
                        ui.separator();

                        ui.heading("⚡ 批量操作");
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            if ui
                                .add_sized(
                                    [95.0, 32.0],
                                    egui::Button::new(
                                        RichText::new("✅ 启用").color(Color32::WHITE),
                                    )
                                    .fill(Color32::from_rgb(60, 140, 90)),
                                )
                                .clicked()
                            {
                                self.batch_enable();
                            }

                            if ui
                                .add_sized(
                                    [95.0, 32.0],
                                    egui::Button::new(
                                        RichText::new("❌ 禁用").color(Color32::WHITE),
                                    )
                                    .fill(Color32::from_rgb(180, 70, 70)),
                                )
                                .clicked()
                            {
                                self.batch_disable();
                            }

                            if ui
                                .add_sized(
                                    [85.0, 32.0],
                                    egui::Button::new("🗑 移除"),
                                )
                                .clicked()
                            {
                                self.remove_selected();
                            }

                            if ui
                                .add_sized(
                                    [85.0, 32.0],
                                    egui::Button::new("🔄 刷新"),
                                )
                                .clicked()
                            {
                                self.sync_states();
                                self.log("状态已刷新", LogLevel::Info);
                            }
                        });

                        ui.add_space(8.0);
                        ui.separator();

                        ui.heading("💾 配置管理");
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            if ui.button("📤 导出配置").clicked() {
                                self.export_config_dialog();
                            }

                            if ui.button("📥 导入配置").clicked() {
                                self.import_config_dialog();
                            }
                        });

                        ui.add_space(8.0);
                        ui.separator();

                        ui.heading("🗂 备份历史");
                        ui.add_space(4.0);

                        let backups = self.backups.clone();

                        if backups.is_empty() {
                            ui.label(RichText::new("暂无备份文件").color(Color32::GRAY));
                        } else {
                            for backup in backups {
                                let filename = backup
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_string();

                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(filename)
                                            .monospace()
                                            .size(11.0),
                                    );

                                    if ui.small_button("恢复").clicked() {
                                        self.restore_backup_path(backup.clone());
                                    }
                                });
                            }
                        }

                        ui.add_space(12.0);
                    });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("📋 操作日志");
            ui.separator();

            ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for log in &self.logs {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("[{}]", log.timestamp))
                                    .monospace()
                                    .color(Color32::GRAY)
                                    .size(11.0),
                            );

                            ui.label(
                                RichText::new(log.level.icon())
                                    .color(log.level.color()),
                            );

                            ui.label(
                                RichText::new(&log.message)
                                    .color(log.level.color()),
                            );
                        });
                    }
                });

            ui.separator();

            ui.horizontal(|ui| {
                if ui.button("🗑 清空日志").clicked() {
                    self.logs.clear();
                }

                if ui.button("📋 复制日志").clicked() {
                    let text = self
                        .logs
                        .iter()
                        .map(|l| format!("[{}] {}", l.timestamp, l.message))
                        .collect::<Vec<_>>()
                        .join("\n");

                    ctx.copy_text(text);
                }
            });
        });

        if self.show_restart_dialog {
            egui::Window::new("⚠ 需要重启")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("注册表修改已成功应用。");
                    ui.label("部分功能需要重启计算机后才能生效。");
                    ui.add_space(12.0);

                    ui.horizontal(|ui| {
                        if ui.button("立即重启").clicked() {
                            self.log("正在请求系统重启...", LogLevel::Warning);

                            std::process::Command::new("shutdown")
                                .args(["/r", "/t", "5"])
                                .spawn()
                                .ok();
                        }

                        if ui.button("稍后重启").clicked() {
                            self.show_restart_dialog = false;
                        }
                    });
                });
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

// ============================================================
// Entry point
// ============================================================

fn main() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 650.0])
            .with_min_inner_size([860.0, 540.0])
            .with_title("APX Feature Toggle Pro v2.0"),
        ..Default::default()
    };

    eframe::run_native(
        "APX Feature Toggle Pro",
        options,
        Box::new(|cc| Box::new(App::new(cc))),
    )
    .map_err(|e| anyhow::anyhow!("GUI 启动失败: {}", e))
}

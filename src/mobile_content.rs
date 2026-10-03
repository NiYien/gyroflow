// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien

pub fn document(kind: &str, language: &str) -> String {
    if kind == "licenses" {
        return include_str!("../resources/legal/mobile-licenses.txt").to_owned();
    }
    let documents: serde_json::Value = serde_json::from_str(include_str!("../resources/legal/mobile-documents.json"))
        .expect("bundled mobile documents must be valid JSON");
    let locale = if language.starts_with("zh") { "zh" } else { "en" };
    documents[locale][kind].as_str().unwrap_or("")
        .replace("{version}", &crate::util::get_version())
}

pub fn prepare_demo() -> Result<String, String> {
    let directory = gyroflow_core::settings::data_dir().join("niyien-demo-v2");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    for (name, data) in [
        ("niyien-demo.mp4", include_bytes!("../resources/demo/niyien-demo.mp4").as_slice()),
        ("niyien-demo.gcsv", include_bytes!("../resources/demo/niyien-demo.gcsv").as_slice()),
    ] {
        let path = directory.join(name);
        if !path.exists() {
            std::fs::write(path, data).map_err(|error| error.to_string())?;
        }
    }
    let url = |name: &str| gyroflow_core::filesystem::path_to_url(&directory.join(name).to_string_lossy());
    let mut project: serde_json::Value = serde_json::from_str(include_str!("../resources/demo/niyien-demo.gyroflow"))
        .map_err(|error| error.to_string())?;
    project["videofile"] = url("niyien-demo.mp4").into();
    project["gyro_source"]["filepath"] = url("niyien-demo.gcsv").into();
    project["output"]["output_folder"] = gyroflow_core::filesystem::path_to_url(&directory.to_string_lossy()).into();
    std::fs::write(directory.join("NiYien-demo.gyroflow"), serde_json::to_vec_pretty(&project).unwrap())
        .map_err(|error| error.to_string())?;
    Ok(url("NiYien-demo.gyroflow"))
}

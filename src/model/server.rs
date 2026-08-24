use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::actor::app::WindowId;
use crate::sys::app::WindowInfo;
use crate::sys::geometry::CGRectDef;
use crate::sys::window_server::WindowServerId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceData {
    pub id: String,
    pub index: usize,
    pub number: usize,
    pub name: String,
    pub is_active: bool,
    pub window_count: usize,
    pub windows: Vec<WindowData>,
}

#[derive(Debug, Clone)]
pub struct WindowData {
    pub id: WindowId,
    pub is_floating: bool,
    pub is_focused: bool,
    pub app_name: Option<String>,
    pub info: WindowInfo,
}

impl Serialize for WindowData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        #[serde_as]
        #[derive(Serialize)]
        struct WindowDataSer<'a> {
            id: WindowId,
            title: &'a str,
            #[serde_as(as = "CGRectDef")]
            frame: &'a objc2_core_foundation::CGRect,
            is_floating: bool,
            is_focused: bool,
            bundle_id: Option<&'a String>,
            app_name: Option<&'a String>,
            window_server_id: Option<u32>,
        }

        let helper = WindowDataSer {
            id: self.id,
            title: &self.info.title,
            frame: &self.info.frame,
            is_floating: self.is_floating,
            is_focused: self.is_focused,
            bundle_id: self.info.bundle_id.as_ref(),
            app_name: self.app_name.as_ref(),
            window_server_id: self.info.sys_id.map(|id| id.as_u32()),
        };

        helper.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WindowData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: Deserializer<'de> {
        #[serde_as]
        #[derive(Deserialize)]
        struct WindowDataDe {
            id: WindowId,
            title: String,
            #[serde_as(as = "CGRectDef")]
            frame: objc2_core_foundation::CGRect,
            is_floating: bool,
            is_focused: bool,
            bundle_id: Option<String>,
            app_name: Option<String>,
            window_server_id: Option<u32>,
        }

        let helper = WindowDataDe::deserialize(deserializer)?;
        let info = WindowInfo {
            is_standard: true,
            is_root: true,
            is_minimized: false,
            is_resizable: true,
            min_size: None,
            max_size: None,
            title: helper.title,
            frame: helper.frame,
            sys_id: helper.window_server_id.map(WindowServerId::new),
            bundle_id: helper.bundle_id,
            path: None,
            ax_role: None,
            ax_subrole: None,
        };

        Ok(WindowData {
            id: helper.id,
            is_floating: helper.is_floating,
            is_focused: helper.is_focused,
            app_name: helper.app_name,
            info,
        })
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use serde_json::json;

    use super::*;

    #[test]
    fn window_data_serializes_with_public_shape() {
        let info = WindowInfo {
            is_standard: true,
            is_root: true,
            is_minimized: false,
            is_resizable: true,
            min_size: None,
            max_size: None,
            title: "Test".to_string(),
            frame: CGRect::new(CGPoint::new(1.0, 2.0), CGSize::new(3.0, 4.0)),
            sys_id: Some(WindowServerId::new(99)),
            bundle_id: Some("com.example.test".to_string()),
            path: None,
            ax_role: None,
            ax_subrole: None,
        };
        let data = WindowData {
            id: WindowId::new(123, 7),
            is_floating: true,
            is_focused: false,
            app_name: Some("Test App".to_string()),
            info,
        };

        let value = serde_json::to_value(&data).expect("serialize WindowData");
        let expected = json!({
            "id": { "pid": 123, "idx": 7 },
            "title": "Test",
            "frame": { "origin": { "x": 1.0, "y": 2.0 }, "size": { "width": 3.0, "height": 4.0 } },
            "is_floating": true,
            "is_focused": false,
            "bundle_id": "com.example.test",
            "app_name": "Test App",
            "window_server_id": 99,
        });
        assert_eq!(value, expected);
    }

}

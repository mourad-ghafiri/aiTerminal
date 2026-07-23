//! The `clip.*` native family — read/write the OS clipboard. A thin, pure wrapper over
//! the `platform::os` clipboard seam (the platform layer owns the OS specifics). Gated
//! by the `clipboard` permission so an app must be granted before it can read or set it.

use corelib::wire::Json;

use super::host::Host;
use super::object::{MethodSpec, NativeObject};
use super::CapCtx;

pub struct ClipObj;

const SPECS: &[MethodSpec] = &[
    MethodSpec { method: "clip.read", describe: "Read the clipboard" },
    MethodSpec { method: "clip.write", describe: "Write the clipboard" },
];

impl NativeObject for ClipObj {
    fn family(&self) -> &'static str {
        "clip"
    }
    fn methods(&self) -> &'static [MethodSpec] {
        SPECS
    }
    fn invoke(&self, method: &str, args: &[(String, String)], _ctx: &CapCtx, _host: &mut dyn Host) -> Result<Json, String> {
        match method {
            "clip.read" => Ok(Json::Str(platform::os::clipboard_read().unwrap_or_default())),
            "clip.write" => {
                let text = args.iter().find(|(k, _)| k == "text").map(|(_, v)| v.as_str()).unwrap_or("");
                platform::os::clipboard_write(text);
                Ok(Json::Bool(true))
            }
            _ => Err(format!("unknown clip method '{method}'")),
        }
    }
}

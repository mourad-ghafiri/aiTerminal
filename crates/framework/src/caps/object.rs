//! The native-object dispatch model (Registry + Strategy patterns).
//!
//! Each capability family is a small value type implementing [`NativeObject`];
//! the [`ObjectRegistry`] owns the set and routes a `family.method` call to the
//! right object. The human description rides on each method's [`MethodSpec`],
//! so adding an object never edits a central `match`
//! (Open/Closed) — register a struct and its methods are live.

use corelib::wire::Json;

use super::host::Host;
use super::CapCtx;

/// Static metadata for one method of a native object.
pub struct MethodSpec {
    /// The fully-qualified method name, e.g. `"sys.run"`.
    pub method: &'static str,
    /// Human description shown in the tool catalog.
    pub describe: &'static str,
}

/// One capability family (`state`-less; pure dispatch over [`CapCtx`]).
pub trait NativeObject: Send + Sync {
    /// The family prefix this object owns (e.g. `"fs"`).
    fn family(&self) -> &'static str;
    /// The methods this object exposes (for consent + describe metadata).
    fn methods(&self) -> &'static [MethodSpec];
    /// Run one `family.method` call. `host` is the (currently empty) host seam;
    /// objects ignore it.
    fn invoke(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, host: &mut dyn Host) -> Result<Json, String>;
}

/// The set of installed native objects + the dispatch over them.
pub struct ObjectRegistry {
    objects: Vec<Box<dyn NativeObject>>,
}

fn family_of(method: &str) -> &str {
    method.split_once('.').map(|(f, _)| f).unwrap_or(method)
}

impl ObjectRegistry {
    /// Build a registry from a set of objects.
    pub fn new(objects: Vec<Box<dyn NativeObject>>) -> Self {
        ObjectRegistry { objects }
    }

    fn object(&self, family: &str) -> Option<&dyn NativeObject> {
        self.objects.iter().find(|o| o.family() == family).map(|b| b.as_ref())
    }

    fn spec(&self, method: &str) -> Option<&MethodSpec> {
        self.object(family_of(method))?.methods().iter().find(|m| m.method == method)
    }

    /// Run a `family.method` call, or an error for an unknown family.
    pub fn run(&self, method: &str, args: &[(String, String)], ctx: &CapCtx, host: &mut dyn Host) -> Result<Json, String> {
        match self.object(family_of(method)) {
            Some(o) => o.invoke(method, args, ctx, host),
            None => Err(format!("unknown capability '{method}'")),
        }
    }

    /// The human description for the tool catalog.
    pub fn describe(&self, method: &str) -> &'static str {
        self.spec(method).map(|m| m.describe).unwrap_or("Native action")
    }

    /// Every registered method's spec, across all families (for the tool catalog).
    pub fn methods(&self) -> impl Iterator<Item = &MethodSpec> {
        self.objects.iter().flat_map(|o| o.methods().iter())
    }
}

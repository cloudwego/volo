//! Install with `volo_build::Builder::thrift().plugin(plugin())` or the original
//! ConfigBuilder/Workspace entry. Runtime dependencies remain separate.

use std::sync::Arc;

use pilota_build::{Context, DefId, Plugin, db::RirDatabase, rir::Item};
pub use volo_build::streamx::{Options, UnaryEnvelopes, UnaryMethod, module_name, unary_envelopes};

pub fn plugin() -> StreamXPlugin {
    StreamXPlugin::default()
}

#[derive(Clone, Default)]
pub struct StreamXPlugin {
    options: Options,
    errors: Vec<String>,
}

impl StreamXPlugin {
    pub fn with_options(options: Options) -> Self {
        Self {
            options,
            errors: Vec::new(),
        }
    }
}

impl Plugin for StreamXPlugin {
    fn on_item(&mut self, cx: &Context, id: DefId, item: Arc<Item>) {
        let Item::Service(service) = &*item else {
            return;
        };
        let result = volo_build::streamx::generate(cx, id, service, &self.options).and_then(
            |(name, code)| {
                cx.with_adjust_mut(id, |adjust| {
                    adjust.add_named_nested_item(name.into(), code.into())
                })
            },
        );
        if let Err(error) = result {
            let file = cx.file_paths()[&cx.node(id).unwrap().file_id]
                .display()
                .to_string();
            self.errors.push(format!(
                "{file}: StreamX service {}: {error:#}",
                service.name
            ));
        }
    }

    fn validate(&self, _: &Context) -> anyhow::Result<()> {
        anyhow::ensure!(self.errors.is_empty(), "{}", self.errors.join("\n"));
        Ok(())
    }
}

//! Code ↔ config bridge (RFC 0003 §7.2): environment variable reads and
//! .NET configuration key reads, string literals only, with file-local
//! evidence (crate docs, *Code ↔ config bridge*).

use rueca_extract::span_of;
use rueca_extract::tree_sitter::Node;
use rueca_model::RefKind;

use crate::extract::{Extractor, simple_name};
use crate::refs::{argument_expr, arguments, string_value};
use crate::types::named_children;

/// Config namespace of .NET configuration keys (`appsettings*.json`).
const DOTNET_NAMESPACE: &str = "dotnet";
/// Types whose indexer and getters read configuration keys
/// (`Microsoft.Extensions.Configuration`).
const CONFIG_TYPES: &[&str] =
    &["ConfigurationManager", "IConfiguration", "IConfigurationManager", "IConfigurationRoot"];
/// Getters whose first argument is a key path.
const CONFIG_GETTERS: &[&str] = &["GetRequiredSection", "GetSection", "GetValue"];

impl<'s> Extractor<'s> {
    /// The string literal value of the first argument of `args`.
    fn first_string(&self, args: Option<Node<'_>>) -> Option<String> {
        let first = arguments(args?).into_iter().next()?;
        string_value(argument_expr(first)?, self.source)
    }

    /// A bridged call (`Environment.GetEnvironmentVariable("X")`,
    /// `config.GetValue<int>("A:B")`, `GetSection`, `GetRequiredSection`,
    /// `GetConnectionString("Db")`): its `EnvVar` / `ConfigKey` reference
    /// instead of a `Call`; `true` when bridged.
    pub(crate) fn bridge_call(
        &mut self,
        name: &str,
        receiver: Option<&str>,
        object: Option<Node<'_>>,
        args: Option<Node<'_>>,
        call: Node<'_>,
    ) -> bool {
        let Some(value) = self.first_string(args) else { return false };
        if name == "GetEnvironmentVariable"
            && matches!(receiver, Some("Environment" | "System.Environment"))
            && !self.declared_types.contains("Environment")
        {
            self.push_ref(RefKind::EnvVar, &value, None, span_of(call), None);
            return true;
        }
        if !object.is_some_and(|o| self.is_config_receiver(receiver.unwrap_or(""), o)) {
            return false;
        }
        let key = match name {
            "GetConnectionString" => format!("ConnectionStrings:{value}"),
            n if CONFIG_GETTERS.contains(&n) => value,
            _ => return false,
        };
        self.push_config_key(&key, call);
        true
    }

    /// `config["A:B"]` on a configuration receiver: a `ConfigKey` read.
    pub(crate) fn bridge_indexer(&mut self, node: Node<'_>) {
        let Some(obj) = node.child_by_field_name("expression") else { return };
        let Some(sub) = node.child_by_field_name("subscript") else { return };
        let receiver = self.receiver(obj);
        if !self.is_config_receiver(&receiver, obj) {
            return;
        }
        let first = named_children(sub).into_iter().find(|c| c.kind() == "argument");
        if let Some(key) = first.and_then(argument_expr).and_then(|e| string_value(e, self.source)) {
            self.push_config_key(&key, node);
        }
    }

    fn push_config_key(&mut self, key: &str, node: Node<'_>) {
        if !key.is_empty() && !key.contains('{') {
            self.push_ref(RefKind::ConfigKey, key, Some(DOTNET_NAMESPACE.into()), span_of(node), None);
        }
    }

    /// A receiver declared as a configuration type, or a `Configuration`
    /// property (`builder.Configuration`, `context.Configuration`, a
    /// `Configuration` member of the enclosing type).
    fn is_config_receiver(&self, receiver: &str, object: Node<'_>) -> bool {
        if let Some(ty) = receiver.strip_prefix("$type:") {
            return CONFIG_TYPES.contains(&simple_name(ty));
        }
        let last = match object.kind() {
            "member_access_expression" => object.child_by_field_name("name").map(|n| self.text(n)),
            "identifier" => Some(self.text(object)),
            _ => None,
        };
        last == Some("Configuration")
    }
}

#[doc(hidden)]
pub fn validate_id(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("id cannot be empty".to_string());
    }
    Ok(())
}

/// Ids that become file and record names may hold only characters that cannot form a path.
#[doc(hidden)]
pub fn validate_message_id(value: &str) -> Result<(), String> {
    validate_id(value)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "id may contain only ASCII letters, digits, hyphens, and underscores".to_string(),
        );
    }
    Ok(())
}

#[doc(hidden)]
pub mod __private {
    pub use serde;
}

/// Defines a string id newtype checked by `$validator`. The invoking crate decides whether the
/// id derives `ts_rs::TS` through its own `typescript` feature.
#[macro_export]
macro_rules! define_id {
    ($name:ident, $validator:path) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
        #[cfg_attr(feature = "typescript", ts(export_to = "types.d.ts"))]
        #[cfg_attr(feature = "typescript", ts(type = "string"))]
        pub struct $name(pub ::std::sync::Arc<str>);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self::try_new(s).expect(concat!(stringify!($name), " contains invalid characters"))
            }

            pub fn try_new(s: impl Into<String>) -> Result<Self, String> {
                let s = s.into();
                $validator(&s)?;
                Ok(Self(::std::sync::Arc::from(s)))
            }

            pub fn as_str(&self) -> &str {
                self.0.as_ref()
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl $crate::__private::serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: $crate::__private::serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> $crate::__private::serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: $crate::__private::serde::Deserializer<'de>,
            {
                let s =
                    <String as $crate::__private::serde::Deserialize>::deserialize(deserializer)?;
                Self::try_new(s).map_err(<D::Error as $crate::__private::serde::de::Error>::custom)
            }
        }
    };
}

define_id!(ScriptExecutionId, validate_message_id);
define_id!(CommandInvocationId, validate_message_id);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_ids_reject_path_syntax() {
        for invalid in [
            "../sessions",
            "/tmp/message",
            r"..\sessions",
            r"C:\temp\message",
            r"\\server\share",
            ".",
        ] {
            assert!(
                ScriptExecutionId::try_new(invalid).is_err(),
                "accepted {invalid:?}"
            );
            assert!(
                CommandInvocationId::try_new(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
        assert!(ScriptExecutionId::try_new("01J_VALID-execution_id").is_ok());
        assert!(CommandInvocationId::try_new("01J_VALID-invocation_id").is_ok());
    }
}

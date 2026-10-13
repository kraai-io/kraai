use crate::{Runtime, wire};
use kraai_runtime::*;
use napi::bindgen_prelude::{Buffer, Either};
use napi_derive::napi;
use serde_json::Value;

macro_rules! node_methods {
    (
        json { $($name:ident($($arg:ident: $ty:ty),* $(,)?) -> $out:ty;)* }
        binary { $($bname:ident($($barg:ident: $bty:ty),*) -> $bout:ty;)* }
    ) => {
        #[napi]
        impl Runtime {
            $(#[napi(skip_typescript)]
            pub async fn $name(&self, $($arg: Value),*) -> napi::Result<Value> {
                $(let $arg: $ty = match wire::decode($arg) {
                    Ok(value) => value,
                    Err(error) => return wire::encode::<$out>(Err(error)),
                };)*
                let result: RuntimeResult<$out> = self.handle.$name($($arg),*).await;
                wire::encode(result)
            })*
        }

        pub(crate) fn declarations(cfg: &ts_rs::Config) -> Result<String, Box<dyn std::error::Error>> {
            use ts_rs::TS;
            let mut output = String::new();
            $(
                export_dependencies::<$out>(cfg)?;
                let args: Vec<String> = vec![$({
                    export_dependencies::<$ty>(cfg)?;
                    format!("{}: {}", stringify!($arg), <$ty>::name(cfg))
                }),*];
                output.push_str(&format!("  {}({}): Promise<RuntimeResult<{}>>;\n",
                    camel_case(stringify!($name)), args.join(", "), <$out>::name(cfg)));
            )*
            output.push_str(BINARY_DECLARATIONS);
            Ok(output)
        }

        #[expect(dead_code, reason = "fails to compile when a listed binary method is missing")]
        fn binary_methods_exist() {
            $(let _ = Runtime::$bname;)*
        }
    };
}

fn export_dependencies<T: ts_rs::TS + ?Sized + 'static>(
    cfg: &ts_rs::Config,
) -> Result<(), ts_rs::ExportError> {
    if T::output_path().is_some() {
        return T::export_all(cfg);
    }
    struct Visitor<'a> {
        cfg: &'a ts_rs::Config,
        result: Result<(), ts_rs::ExportError>,
    }
    impl ts_rs::TypeVisitor for Visitor<'_> {
        fn visit<T: ts_rs::TS + ?Sized + 'static>(&mut self) {
            if self.result.is_ok() {
                self.result = export_dependencies::<T>(self.cfg);
            }
        }
    }
    let mut visitor = Visitor {
        cfg,
        result: Ok(()),
    };
    T::visit_dependencies(&mut visitor);
    T::visit_generics(&mut visitor);
    visitor.result
}

fn camel_case(name: &str) -> String {
    let mut uppercase = false;
    name.chars()
        .filter_map(|character| {
            if character == '_' {
                uppercase = true;
                None
            } else if uppercase {
                uppercase = false;
                Some(character.to_ascii_uppercase())
            } else {
                Some(character)
            }
        })
        .collect()
}

kraai_runtime::runtime_methods!(node_methods);

const BINARY_DECLARATIONS: &str = "  importImage(bytes: Uint8Array): Promise<RuntimeResult<ImageAttachment>>;\n  readImage(image: ImageAttachment): Promise<RuntimeResult<Uint8Array>>;\n";

/// A successful result whose value is raw bytes, shaped like `RuntimeResult`.
#[napi(object)]
pub struct BytesOk {
    #[napi(js_name = "Ok")]
    pub ok: Buffer,
}

#[napi]
impl Runtime {
    #[napi(skip_typescript)]
    pub async fn import_image(&self, bytes: Buffer) -> napi::Result<Value> {
        wire::encode(self.handle.import_image(bytes.to_vec()).await)
    }

    #[napi(skip_typescript)]
    pub async fn read_image(&self, image: Value) -> napi::Result<Either<BytesOk, Value>> {
        let result = match wire::decode(image) {
            Ok(image) => self.handle.read_image(image).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(bytes) => Ok(Either::A(BytesOk { ok: bytes.into() })),
            Err(error) => wire::encode::<()>(Err(error)).map(Either::B),
        }
    }
}

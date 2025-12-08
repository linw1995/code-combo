use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, LitStr, parse_macro_input};

/// Derive macro for implementing the `Identity` trait and registering a component.
///
/// This macro generates:
/// - An implementation of the `Identity` trait that returns the specified type ID
/// - A registration module that automatically registers the component with the session system
///
/// # Attributes
///
/// The macro requires a `#[component]` attribute with a `type_id` parameter:
/// ```ignore
/// #[derive(ComponentExt)]
/// #[component(type_id = "my_component")]
/// struct MyComponent;
/// ```
///
/// # Generated Code
///
/// The macro generates:
/// - `impl Identity for MyComponent` with the `id()` method returning the type ID
/// - A hidden module that registers the component during static initialization
#[proc_macro_derive(ComponentExt, attributes(component))]
pub fn component(input: TokenStream) -> TokenStream {
    let mut type_id: Option<LitStr> = None;

    let DeriveInput {
        ident,
        attrs,
        generics,
        ..
    } = parse_macro_input!(input);
    let attr = attrs
        .iter()
        .find(|a| a.path().is_ident("component"))
        .expect("`component` attribute is required");
    attr.parse_nested_meta(|m| {
        if m.path.is_ident("type_id") {
            let value = m.value().expect("`type_id` assignment is required");
            let value = value
                .parse::<LitStr>()
                .expect("`type_id` value must be a literal string");
            type_id.replace(value);
        }
        Ok(())
    })
    .ok();

    let type_id = type_id.expect("`type_id` assignment not found");
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let handler_mod = quote::format_ident!("__COMPONENT_REGISTER_{}", ident);
    let expanded = quote! {
        impl #impl_generics crate::components::Identity for #ident #ty_generics #where_clause {
            fn id(&self) -> &'static str {
                #type_id
            }
        }

        #[allow(non_snake_case)]
        mod #handler_mod {
            extern crate ctor;

            use super::*;

            #[ctor::ctor]
            fn init() {
                use crate::{
                    components::Component,
                    session::{Session, register_component},
                };

                register_component(
                    #type_id,
                    |s: Session| -> Result<Box<dyn Component>> {
                        #ident::load(s).map(|x|x.into())
                    },
                );
            }
        }
    };

    TokenStream::from(expanded)
}

/// Derive macro for registering a content component.
///
/// This macro generates a registration module that automatically registers
/// a content component with the session system during static initialization.
///
/// # Attributes
///
/// The macro requires a `#[component]` attribute with a `type_id` parameter:
/// ```ignore
/// #[derive(ComponentExt, ContentComponentExt)]
/// #[component(type_id = "my_content_component")]
/// struct MyContentComponent;
/// ```
///
/// # Generated Code
///
/// The macro generates a hidden module that registers the content component
/// during static initialization using the `ctor` crate.
#[proc_macro_derive(ContentComponentExt)]
pub fn content_component(input: TokenStream) -> TokenStream {
    let mut type_id: Option<LitStr> = None;

    let DeriveInput { ident, attrs, .. } = parse_macro_input!(input);
    let attr = attrs
        .iter()
        .find(|a| a.path().is_ident("component"))
        .expect("`component` attribute is required");
    attr.parse_nested_meta(|m| {
        if m.path.is_ident("type_id") {
            let value = m.value().expect("`type_id` assignment is required");
            let value = value
                .parse::<LitStr>()
                .expect("`type_id` value must be a literal string");
            type_id.replace(value);
        }
        Ok(())
    })
    .ok();

    let type_id = type_id.expect("`type_id` assignment not found");
    let handler_mod = quote::format_ident!("__CONTENT_COMPONENT_REGISTER_{}", ident);
    let expanded = quote! {
        #[allow(non_snake_case)]
        mod #handler_mod {
            extern crate ctor;

            use super::*;

            #[ctor::ctor]
            fn init() {
                use crate::{
                    components::ContentComponent,
                    session::{Session, register_content_component},
                };

                register_content_component(
                    #type_id,
                    |s: Session| -> Result<Box<dyn ContentComponent>> {
                        #ident::load(s).map(|x|x.into())
                    },
                );
            }
        }
    };

    TokenStream::from(expanded)
}

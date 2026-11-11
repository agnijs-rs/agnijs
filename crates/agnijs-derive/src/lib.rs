//! Macros for the `agnijs` embedding crate. Use them through `agnijs`
//! (`agnijs::HostClass`, `agnijs::host_methods`); this crate is its
//! implementation detail.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    parse_macro_input, DeriveInput, Expr, FnArg, ImplItem, ImplItemFn, ItemImpl, Lit, Pat,
    ReturnType, Type,
};

/// Make a struct a JavaScript host class.
///
/// Implements `agnijs::HostObject` and `agnijs::HostClass` for the type. The
/// JavaScript class name is the type name, or `#[agnijs(name = "Other")]`.
/// Pair it with [`host_methods`] on an `impl` block to give the class a
/// constructor and methods.
#[proc_macro_derive(HostClass, attributes(agnijs))]
pub fn derive_host_class(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let ident = &input.ident;
    let mut name = ident.to_string();
    for attr in &input.attrs {
        if attr.path().is_ident("agnijs") {
            let r = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let v: Expr = meta.value()?.parse()?;
                    if let Expr::Lit(l) = &v {
                        if let Lit::Str(s) = &l.lit {
                            name = s.value();
                            return Ok(());
                        }
                    }
                    return Err(meta.error("name must be a string literal"));
                }
                Err(meta.error("unknown agnijs option; expected `name = \"...\"`"))
            });
            if let Err(e) = r {
                return e.to_compile_error().into();
            }
        }
    }
    let (impl_g, ty_g, where_g) = input.generics.split_for_impl();
    quote! {
        impl #impl_g ::agnijs::HostObject for #ident #ty_g #where_g {
            fn as_any(&self) -> &dyn ::std::any::Any { self }
            fn as_any_mut(&mut self) -> &mut dyn ::std::any::Any { self }
        }
        impl #impl_g ::agnijs::HostClass for #ident #ty_g #where_g {
            const NAME: &'static str = #name;
        }
    }
    .into()
}

/// Expose an `impl` block to JavaScript.
///
/// - A function without a receiver named `new`, or marked `#[constructor]`,
///   becomes the JavaScript constructor. It returns `Self` or
///   `Result<Self, E>` where `E: Into<agnijs::JsError>`.
/// - Methods taking `&self` or `&mut self` become prototype methods under a
///   camelCase name.
/// - Every other function is left alone.
///
/// Parameters are decoded from the JavaScript arguments with serde, so each
/// type implements `serde::Deserialize`; return types implement
/// `serde::Serialize`, or are `Result<T, E>` with `E: Into<agnijs::JsError>`,
/// in which case `Err` is thrown. A missing argument reads as `undefined`
/// (so `Option<T>` parameters are optional).
#[proc_macro_attribute]
pub fn host_methods(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut imp = parse_macro_input!(item as ItemImpl);
    match expand_methods(&mut imp) {
        Ok(extra) => quote! { #imp #extra }.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

#[derive(PartialEq)]
enum Ret {
    Unit,
    Plain,
    ResultUnit,
    Result,
}

fn classify(ret: &ReturnType) -> Ret {
    match ret {
        ReturnType::Default => Ret::Unit,
        ReturnType::Type(_, ty) => match &**ty {
            Type::Tuple(t) if t.elems.is_empty() => Ret::Unit,
            Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Result") => {
                let seg = p.path.segments.last().unwrap();
                if let syn::PathArguments::AngleBracketed(a) = &seg.arguments {
                    if let Some(syn::GenericArgument::Type(Type::Tuple(t))) = a.args.first() {
                        if t.elems.is_empty() {
                            return Ret::ResultUnit;
                        }
                    }
                }
                Ret::Result
            }
            _ => Ret::Plain,
        },
    }
}

fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for c in name.chars() {
        if c == '_' {
            up = !out.is_empty();
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Statements decoding the parameters after the receiver, and the idents.
fn decode_args(
    f: &ImplItemFn,
    skip_receiver: bool,
) -> syn::Result<(Vec<TokenStream2>, Vec<syn::Ident>)> {
    let mut stmts = Vec::new();
    let mut names = Vec::new();
    let mut i = 0usize;
    for arg in &f.sig.inputs {
        match arg {
            FnArg::Receiver(_) => {
                if !skip_receiver {
                    return Err(syn::Error::new_spanned(arg, "unexpected receiver"));
                }
            }
            FnArg::Typed(pt) => {
                let ident = match &*pt.pat {
                    Pat::Ident(p) => p.ident.clone(),
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "host method parameters must be plain identifiers",
                        ))
                    }
                };
                let ty = &pt.ty;
                stmts.push(quote! {
                    let #ident: #ty = ::agnijs::__private::arg(__args, #i)?;
                });
                names.push(ident);
                i += 1;
            }
        }
    }
    Ok((stmts, names))
}

fn ret_tokens(kind: &Ret, call: TokenStream2) -> TokenStream2 {
    match kind {
        Ret::Unit => quote! { { #call; Ok(None) } },
        Ret::Plain => quote! { ::agnijs::__private::ret_plain(#call) },
        Ret::ResultUnit => quote! { ::agnijs::__private::ret_result_unit(#call) },
        Ret::Result => quote! { ::agnijs::__private::ret_result(#call) },
    }
}

fn expand_methods(imp: &mut ItemImpl) -> syn::Result<TokenStream2> {
    let self_ty = (*imp.self_ty).clone();
    let mut regs = Vec::new();
    for item in &mut imp.items {
        let ImplItem::Fn(f) = item else { continue };
        let is_ctor_attr = f.attrs.iter().any(|a| a.path().is_ident("constructor"));
        f.attrs.retain(|a| !a.path().is_ident("constructor"));
        let has_receiver = f.sig.receiver().is_some();
        let fname = f.sig.ident.clone();
        let kind = classify(&f.sig.output);
        if !has_receiver && (is_ctor_attr || fname == "new") {
            let (stmts, names) = decode_args(f, false)?;
            let body = match kind {
                Ret::Plain => quote! { Ok(<#self_ty>::#fname(#(#names),*)) },
                Ret::Result => quote! {
                    <#self_ty>::#fname(#(#names),*).map_err(::std::convert::Into::<::agnijs::JsError>::into)
                },
                _ => {
                    return Err(syn::Error::new_spanned(
                        &f.sig.output,
                        "a constructor returns Self or Result<Self, E>",
                    ))
                }
            };
            regs.push(quote! {
                __b.constructor(|__args: &[::agnijs::Json]| -> ::std::result::Result<Self, ::agnijs::JsError> {
                    #(#stmts)*
                    #body
                });
            });
        } else if has_receiver {
            let (stmts, names) = decode_args(f, true)?;
            let js = camel(&fname.to_string());
            let call = quote! { __this.#fname(#(#names),*) };
            let body = ret_tokens(&kind, call);
            regs.push(quote! {
                __b.method(#js, |__this: &mut Self, __args: &[::agnijs::Json]|
                    -> ::std::result::Result<::std::option::Option<::agnijs::Json>, ::agnijs::JsError> {
                    #(#stmts)*
                    #body
                });
            });
        }
    }
    Ok(quote! {
        impl ::agnijs::HostClassMethods for #self_ty {
            fn build(__b: &mut ::agnijs::ClassBuilder<Self>) {
                #(#regs)*
            }
        }
    })
}

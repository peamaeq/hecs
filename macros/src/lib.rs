extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// Implement `Bundle` for a monomorphic struct
///
/// Using derived `Bundle` impls improves spawn performance and can be convenient when combined with
/// other derives like `serde::Deserialize`.
///
/// ```
/// # use hecs::*;
/// # struct MeshId(&'static str);
/// # #[derive(Copy, Clone, PartialEq, Debug)]
/// # struct Position([f32; 3]);
/// #[derive(Bundle)]
/// struct StaticMesh {
///     mesh: MeshId,
///     position: Position,
/// }
/// let mut world = World::new();
/// let position = Position([1.0, 2.0, 3.0]);
/// let e = world.spawn(StaticMesh { position, mesh: MeshId("example.gltf") });
/// assert_eq!(*world.get::<Position>(e).unwrap(), position);
/// ```
#[proc_macro_derive(Bundle)]
pub fn derive_bundle(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    if !input.generics.params.is_empty() {
        return TokenStream::from(
            quote! { compile_error!("derive(Bundle) does not support generics"); },
        );
    }
    let data = match input.data {
        syn::Data::Struct(s) => s,
        _ => {
            return TokenStream::from(
                quote! { compile_error!("derive(Bundle) only supports structs"); },
            )
        }
    };
    let ident = input.ident;
    let (tys, fields) = struct_fields(&data.fields);

    let n = tys.len();
    let code = quote! {
        impl ::hecs::Bundle for #ident {
            fn elements() -> &'static [std::any::TypeId] {
                use std::any::TypeId;
                use std::mem;

                use ::hecs::once_cell::sync::Lazy;

                static ELEMENTS: Lazy<[TypeId; #n]> = Lazy::new(|| {
                    let mut dedup = std::collections::HashSet::new();
                    for &(ty, name) in [#((std::any::TypeId::of::<#tys>(), std::any::type_name::<#tys>())),*].iter() {
                        if !dedup.insert(ty) {
                            panic!("{} has multiple {} fields; each type must occur at most once!", stringify!(#ident), name);
                        }
                    }

                    let mut tys = [#((mem::align_of::<#tys>(), TypeId::of::<#tys>())),*];
                    tys.sort_unstable_by(|x, y| x.0.cmp(&y.0).reverse().then(x.1.cmp(&y.1)));
                    let mut ids = [TypeId::of::<()>(); #n];
                    for (id, info) in ids.iter_mut().zip(tys.iter()) {
                        *id = info.1;
                    }
                    ids
                });
                &*ELEMENTS
            }
        }

        impl ::hecs::DynamicBundle for #ident {
            fn get_archetype(&self, table: &mut ::hecs::ArchetypeTable) -> u32 {
                table
                    .get_id(Self::elements())
                    .unwrap_or_else(|| {
                        let mut info = vec![#(::hecs::TypeInfo::of::<#tys>()),*];
                        info.sort_unstable();
                        table.alloc(info)
                    })
            }

            unsafe fn store(self, archetype: &mut ::hecs::Archetype, index: u32) {
                #(
                    archetype.put(self.#fields, index);
                )*
            }
        }
    };
    TokenStream::from(code)
}

/// Implement `Query` for a struct whose fields are queries
///
/// ```
/// # use hecs::*;
/// #[derive(Query, PartialEq, Debug)]
/// struct MyQuery<'a> {
///     foo: &'a i32,
///     bar: Option<&'a mut bool>,
/// }
/// let mut world = World::new();
/// let e = world.spawn((42,));
/// assert_eq!(world.query::<MyQuery>().collect::<Vec<_>>(), &[(e, MyQuery { foo: &42, bar: None })]);
/// ```
#[proc_macro_derive(Query)]
pub fn derive_query(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let lifetimes = input.generics.lifetimes().collect::<Vec<_>>();

    let lifetime = match lifetimes[..] {
        [x] => x.lifetime.clone(),
        _ => {
            return TokenStream::from(
                quote! { compile_error!("derive(Query) must be applied to structs with exactly one unbounded lifetime parameter"); },
            );
        }
    };
    if input.generics.where_clause.is_some() {
        return TokenStream::from(
            quote! { compile_error!("derive(Query) does not support where clauses"); },
        );
    }
    let data = match input.data {
        syn::Data::Struct(s) => s,
        _ => {
            return TokenStream::from(
                quote! { compile_error!("derive(Query) only supports structs"); },
            )
        }
    };
    let ident = input.ident;
    let vis = input.vis;
    let fetch = syn::Ident::new(&format!("{}Fetch", ident), Span::call_site());

    let (tys, fields) = struct_fields(&data.fields);

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let fetch_def = match data.fields {
        syn::Fields::Named(_) => quote! {
            #[doc(hidden)]
            #vis struct #fetch #ty_generics #where_clause {
                #(
                    #fields: <#tys as Query<#lifetime>>::Fetch,
                )*
            }
        },
        syn::Fields::Unnamed(_) => quote! {
            #[doc(hidden)]
            #vis struct #fetch #ty_generics (
                #(
                    #fields: <#tys as Query<#lifetime>>::Fetch,
                )*
            ) #where_clause;
        },
        syn::Fields::Unit => quote! { struct #fetch #ty_generics #where_clause {} },
    };

    let code = quote! {
        #fetch_def

        impl #impl_generics ::hecs::Fetch<#lifetime> for #fetch #ty_generics #where_clause {
            type Item = #ident #ty_generics;

            fn get(archetype: & #lifetime Archetype) -> Option<Self> {
                Some(Self {
                    #(
                        #fields: <#tys as Query<#lifetime>>::Fetch::get(archetype)?,
                    )*
                })
            }

            unsafe fn next(&mut self) -> Self::Item {
                #ident {
                    #(
                        #fields: self.#fields.next(),
                    )*
                }
            }
        }

        impl #impl_generics ::hecs::Query<#lifetime> for #ident #ty_generics #where_clause {
            type Fetch = #fetch #ty_generics;

            fn borrow(state: &BorrowState) {
                #(
                    <#tys as Query>::borrow(state);
                )*
            }

            fn release(state: &BorrowState) {
                #(
                    <#tys as Query>::release(state);
                )*
            }
        }
    };
    TokenStream::from(code)
}

fn struct_fields(fields: &syn::Fields) -> (Vec<&syn::Type>, Vec<syn::Ident>) {
    match fields {
        syn::Fields::Named(ref fields) => fields
            .named
            .iter()
            .map(|f| (&f.ty, f.ident.clone().unwrap()))
            .unzip(),
        syn::Fields::Unnamed(ref fields) => fields
            .unnamed
            .iter()
            .enumerate()
            .map(|(i, f)| (&f.ty, syn::Ident::new(&i.to_string(), Span::call_site())))
            .unzip(),
        syn::Fields::Unit => (Vec::new(), Vec::new()),
    }
}
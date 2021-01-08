//! Macros that help define communication between a wasm blob and its
//! rust host.
//!
//! All the types are being exchanged bincode-encoded. If multiple
//! parameters are to be taken, they are exchanged as a
//! bincode-encoded tuple.
//!
//! `&mut` references taken as arguments are taken as though they were
//! by value, and then returned as supplementary arguments in a tuple.
//!
//! Note: this crate's guest-side implementation is very heavily tied
//! to the `kannader_config` crate's implementation. This is on
//! purpose and the two crates should be used together. They are split
//! only for technical reasons.

// The functions are all implemented in wasm with:
//
// Parameters: (address, size) of the allocated block containing
// the serialized message. Ownership is passed to the called
// function.
//
// Return: u64 whose upper 32 bits are the size and lower 32 bits
// the address of a block containing the serialized message.
// Ownership is passed to the caller function.

// TODO: this all should be auto-generated by wasm-bindgen, wasm
// interface types, wiggle or similar

use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;

#[proc_macro]
pub fn implement_guest(input: TokenStream) -> TokenStream {
    let cfg = syn::parse_macro_input!(input as Ident);
    let res = quote! {
        #[no_mangle]
        pub unsafe extern "C" fn allocate(size: usize) -> usize {
            // TODO: handle alloc error (ie. null return) properly (trap?)
            unsafe {
                std::alloc::alloc(std::alloc::Layout::from_size_align_unchecked(size, 8)) as usize
            }
        }

        #[no_mangle]
        pub unsafe extern "C" fn deallocate(ptr: usize, size: usize) {
            unsafe {
                std::alloc::dealloc(
                    ptr as *mut u8,
                    std::alloc::Layout::from_size_align_unchecked(size, 8),
                )
            }
        }

        std::thread_local! {
            static KANNADER_CFG: std::cell::RefCell<Option<#cfg>> =
                std::cell::RefCell::new(None);
        }

        // TODO: handle errors properly here too (see the TODO down the file)
        #[no_mangle]
        pub unsafe extern "C" fn setup(ptr: usize, size: usize) {
            // Recover the argument
            let arg_slice = std::slice::from_raw_parts(ptr as *const u8, size);
            let path: std::path::PathBuf = bincode::deserialize(arg_slice).unwrap();

            // Deallocate the memory block
            deallocate(ptr, size);

            // Run the code
            KANNADER_CFG.with(|cfg| {
                assert!(cfg.borrow().is_none());
                *cfg.borrow_mut() = Some(<#cfg as kannader_config::Config>::setup(path));
            })
        }

        #[allow(unused)]
        fn DID_YOU_CALL_implement_guest_MACRO() {
            DID_YOU_CALL_server_config_implement_guest_server_MACRO();
        }
    };
    res.into()
}

#[proc_macro]
pub fn implement_host(_input: TokenStream) -> TokenStream {
    let res = quote! {
        use std::{path::Path, rc::Rc};

        use anyhow::{anyhow, ensure, Context};

        // TODO: take struct name as argument instead of forcing the caller to put in a
        // mod (and same below)
        // TODO: factor code out with the below similar code to serialize the argument
        // TODO: make sure we deallocate the buffers in case any error happens (note
        // however that currently wasm supports only panic=abort which generates a trap,
        // so handling panics in wasm properly must be left for later when this is fixed
        // upstream)
        pub fn setup(
            path: &Path,
            instance: &wasmtime::Instance,
            allocate: Rc<dyn Fn(u32) -> Result<u32, wasmtime::Trap>>,
        ) -> anyhow::Result<()> {
            // Recover memory instance
            let memory = instance
                .get_memory("memory")
                .ok_or_else(|| anyhow!("Failed to find memory export ‘memory’"))?;

            // Recover setup function
            let wasm_fun = instance
                .get_func("setup")
                .ok_or_else(|| anyhow!("Failed to find function export ‘setup’"))?
                .get2()
                .with_context(|| format!("Checking the type of ‘setup’"))?;

            fn force_type<F: Fn(u32, u32) -> Result<(), wasmtime::Trap>>(_: &F) {}
            force_type(&wasm_fun);

            // Compute size of function
            let arg_size: u64 = bincode::serialized_size(path)
                .context("Figuring out size to allocate for argument buffer for ‘setup’")?;
            debug_assert!(
                arg_size <= u32::MAX as u64,
                "Message size above u32::MAX, something is really wrong"
            );
            let arg_size = arg_size as u32;

            // Allocate argument buffer
            let arg_ptr = allocate(arg_size).context("Allocating argument buffer for ‘setup’")?;
            ensure!(
                (arg_ptr as usize).saturating_add(arg_size as usize) < memory.data_size(),
                "Wasm allocator returned allocation outside of its memory"
            );

            // Serialize to argument buffer
            let arg_vec =
                bincode::serialize(path).context("Serializing argument buffer for ‘setup’")?;
            debug_assert_eq!(
                arg_size as usize,
                arg_vec.len(),
                "bincode-computed size is {} but actual size is {}",
                arg_size,
                arg_vec.len()
            );
            unsafe {
                // TODO: these volatile copies are not actually
                // required, wasm threads will not happen without some
                // special thing being enabled on the memory.
                std::intrinsics::volatile_copy_nonoverlapping_memory(
                    memory.data_ptr().add(arg_ptr as usize),
                    arg_vec.as_ptr(),
                    arg_size as usize,
                );
            }

            // Call the function
            let () = wasm_fun(arg_ptr, arg_size).context("Running wasm function ‘setup’")?;

            Ok(())
        }
    };
    res.into()
}

struct Communicator {
    trait_name: Ident,
    did_you_call_fn_name: Ident,
    funcs: Vec<Function>,
}

struct Function {
    ffi_name: Ident,
    fn_name: Ident,
    args: Vec<Argument>,
    ret: proc_macro2::TokenStream,
    terminator: proc_macro2::TokenStream,
}

struct Argument {
    name: Ident,
    is_mut: bool,
    ty: proc_macro2::TokenStream,
}

#[proc_macro]
pub fn server_config_implement_trait(_input: TokenStream) -> TokenStream {
    make_trait(SERVER_CONFIG())
}

#[proc_macro]
pub fn server_config_implement_guest_server(input: TokenStream) -> TokenStream {
    let impl_name = syn::parse_macro_input!(input as Ident);
    make_guest_server(impl_name, SERVER_CONFIG())
}

#[proc_macro]
pub fn server_config_implement_host_client(input: TokenStream) -> TokenStream {
    let struct_name = syn::parse_macro_input!(input as Ident);
    make_host_client(struct_name, SERVER_CONFIG())
}

fn make_trait(c: Communicator) -> TokenStream {
    let trait_name = c.trait_name;
    let funcs = c.funcs.into_iter().map(|f| {
        let fn_name = f.fn_name;
        let ret = f.ret;
        let terminator = f.terminator;
        let args = f.args.into_iter().map(|Argument { name, is_mut, ty }| {
            if is_mut {
                quote!(#name: &mut #ty)
            } else {
                quote!(#name: #ty)
            }
        });
        quote! {
            #[allow(unused_variables)]
            fn #fn_name(cfg: &Self::Cfg, #(#args),*) -> #ret
                #terminator
        }
    });
    let res = quote! {
        pub trait #trait_name {
            type Cfg: Config;

            #(#funcs)*
        }
    };
    res.into()
}

fn make_guest_server(impl_name: Ident, c: Communicator) -> TokenStream {
    let Communicator {
        trait_name,
        did_you_call_fn_name,
        funcs,
        ..
    } = c;
    let funcs = funcs.into_iter().map(
        |Function {
             ffi_name,
             fn_name,
             args,
             ..
         }| {
            let deserialize_pat = args.iter().map(|Argument { name, is_mut, .. }| {
                if *is_mut {
                    quote!(mut #name)
                } else {
                    quote!(#name)
                }
            });
            let arguments = args.iter().map(|Argument { name, is_mut, .. }| {
                if *is_mut {
                    quote!(&mut #name)
                } else {
                    quote!(#name)
                }
            });
            let result = args.iter().filter_map(
                |Argument { name, is_mut, .. }| {
                    if *is_mut { Some(quote!(#name)) } else { None }
                },
            );
            quote! {
                // TODO: handle errors properly (but what does “properly”
                // exactly mean here? anyway, probably not `.unwrap()` /
                // `assert!`...) (and above in the file too)
                #[no_mangle]
                pub unsafe fn #ffi_name(arg_ptr: usize, arg_size: usize) -> u64 {
                    // Deserialize from the argument slice
                    let arg_slice = std::slice::from_raw_parts(arg_ptr as *const u8, arg_size);
                    let ( #(#deserialize_pat),* ) = bincode::deserialize(arg_slice).unwrap();

                    // Deallocate the argument slice
                    deallocate(arg_ptr, arg_size);

                    // Call the callback
                    let res = KANNADER_CFG.with(|cfg| {
                        <#impl_name as kannader_config::#trait_name>::#fn_name(
                            cfg.borrow().as_ref().unwrap(),
                            #(#arguments),*
                        )
                    });
                    let res = (res, #(#result),*);

                    // Allocate return buffer
                    let ret_size: u64 = bincode::serialized_size(&res).unwrap();
                    debug_assert!(
                        ret_size <= usize::MAX as u64,
                        "Message size above usize::MAX, something is really wrong"
                    );
                    let ret_size: usize = ret_size as usize;
                    let ret_ptr: usize = allocate(ret_size);
                    let ret_slice = std::slice::from_raw_parts_mut(ret_ptr as *mut u8, ret_size);

                    // Serialize the result to the return buffer
                    bincode::serialize_into(ret_slice, &res).unwrap();

                    // We know that usize is u32 thanks to the above const_assert
                    ((ret_size as u64) << 32) | (ret_ptr as u64)
                }
            }
        },
    );
    let res = quote! {
        #(#funcs)*

        #[allow(unused)]
        fn #did_you_call_fn_name() {
            DID_YOU_CALL_implement_guest_MACRO();
        }
    };
    res.into()
}

fn make_host_client(struct_name: Ident, c: Communicator) -> TokenStream {
    let func_defs = c.funcs.iter().map(|f| {
        let fn_name = &f.fn_name;
        let ret = &f.ret;
        let args = f.args.iter().map(|a| {
            let ty = &a.ty;
            if a.is_mut {
                quote!(&mut #ty)
            } else {
                quote!(#ty)
            }
        });
        quote! {
            pub #fn_name: Box<dyn Fn(#(#args),*) -> anyhow::Result<#ret>>,
        }
    });
    let func_gets = c.funcs.iter().map(|f| {
        let fn_name = &f.fn_name;
        let ffi_name_str = format!("{}", f.ffi_name);
        let host_args = f.args.iter().map(|Argument { name, is_mut, ty }| {
            if *is_mut {
                quote!(#name: &mut #ty)
            } else {
                quote!(#name: #ty)
            }
        });
        let encode_args = f.args.iter().map(|Argument { name, .. }| quote!(&#name));
        let result_assignment = f.args.iter().filter_map(|a| {
            let name = &a.name;
            if a.is_mut { Some(quote!(*#name)) } else { None }
        });
        let failed_to_find_export = format!("Failed to find function export ‘{}’", f.ffi_name);
        let checking_type = format!("Checking the type of ‘{}’", f.ffi_name);
        let figuring_out_size_to_allocate_for_arg_buf = format!(
            "Figuring out size to allocate for argument buffer for ‘{}’",
            f.ffi_name
        );
        let allocating_arg_buf = format!("Allocating argument buffer for ‘{}’", f.ffi_name);
        let serializing_arg_buf = format!("Serializing argument buffer for ‘{}’", f.ffi_name);
        let running_wasm_func = format!("Running wasm function ‘{}’", f.ffi_name);
        let returned_alloc_outside_of_memory = format!(
            "Wasm function ‘{}’ returned allocation outside of its memory",
            f.ffi_name,
        );
        let deallocating_ret_buf =
            format!("Deallocating return buffer for function ‘{}’", f.ffi_name);
        let deserializing_ret_msg = format!("Deserializing return message of ‘{}’", f.ffi_name);
        quote! {
            let #fn_name = {
                let memory = memory.clone();
                let allocate = allocate.clone();
                let deallocate = deallocate.clone();

                let wasm_fun = instance
                    .get_func(#ffi_name_str)
                    .ok_or_else(|| anyhow::Error::msg(#failed_to_find_export))?
                    .get2()
                    .context(#checking_type)?;

                fn force_type<F: Fn(u32, u32) -> Result<u64, wasmtime::Trap>>(_: &F) {}
                force_type(&wasm_fun);

                Box::new(move |#(#host_args),*| {
                    // Get the to-be-encoded argument
                    let arg = ( #(#encode_args),* );

                    // Compute the size of the argument
                    let arg_size: u64 = bincode::serialized_size(&arg)
                        .context(#figuring_out_size_to_allocate_for_arg_buf)?;
                    debug_assert!(
                        arg_size <= u32::MAX as u64,
                        "Message size above u32::MAX, something is really wrong"
                    );
                    let arg_size = arg_size as u32;

                    // Allocate argument buffer
                    let arg_ptr = allocate(arg_size).context(#allocating_arg_buf)?;
                    ensure!(
                        (arg_ptr as usize).saturating_add(arg_size as usize) < memory.data_size(),
                        "Wasm allocator returned allocation outside of its memory"
                    );

                    // Serialize to argument buffer
                    // TODO: implement io::Write for a VolatileWriter that directly
                    // volatile-copies the message bytes to wasm memory
                    let arg_vec = bincode::serialize(&arg).context(#serializing_arg_buf)?;
                    debug_assert_eq!(
                        arg_size as usize,
                        arg_vec.len(),
                        "bincode-computed size is {} but actual size is {}",
                        arg_size,
                        arg_vec.len()
                    );
                    unsafe {
                        std::intrinsics::volatile_copy_nonoverlapping_memory(
                            memory.data_ptr().add(arg_ptr as usize),
                            arg_vec.as_ptr(),
                            arg_size as usize,
                        );
                    }

                    // Call the function
                    let res_u64 = wasm_fun(arg_ptr, arg_size).context(#running_wasm_func)?;
                    let res_ptr = (res_u64 & 0xFFFF_FFFF) as usize;
                    let res_size = ((res_u64 >> 32) & 0xFFFF_FFFF) as usize;
                    ensure!(
                        res_ptr.saturating_add(res_size) < memory.data_size(),
                        #returned_alloc_outside_of_memory
                    );

                    // Recover the return slice
                    // TODO: implement io::Read for a VolatileReader that directly volatile-copies
                    // the message bytes from wasm memory
                    let mut res_msg = vec![0; res_size];
                    unsafe {
                        std::intrinsics::volatile_copy_nonoverlapping_memory(
                            res_msg.as_mut_ptr(),
                            memory.data_ptr().add(res_ptr),
                            res_size,
                        );
                    }

                    // Deallocate the return slice
                    deallocate(res_ptr as u32, res_size as u32).context(#deallocating_ret_buf)?;

                    // Read the result
                    let res;
                    (res, #(#result_assignment),*) = bincode::deserialize(&res_msg)
                        .context(#deserializing_ret_msg)?;
                    Ok(res)
                })
            };
        }
    });
    let func_names = c.funcs.iter().map(|f| &f.fn_name);
    let res = quote! {
        pub struct #struct_name {
            #(#func_defs)*
        }

        impl #struct_name {
            pub fn build(
                instance: &wasmtime::Instance,
                allocate: std::rc::Rc<dyn Fn(u32) -> Result<u32, wasmtime::Trap>>,
                deallocate: std::rc::Rc<dyn Fn(u32, u32) -> Result<(), wasmtime::Trap>>,
            ) -> anyhow::Result<Self> {
                use anyhow::{anyhow, ensure, Context};

                let memory = instance
                    .get_memory("memory")
                    .ok_or_else(|| anyhow!("Failed to find memory export ‘memory’"))?;

                #(#func_gets)*

                Ok(Self { #(#func_names),* })
            }
        }
    };
    res.into()
}

macro_rules! communicator {
    (@is_mut ()) => { false };
    (@is_mut (&mut)) => { true };

    (
        communicator $trait_name:ident
            $did_you_call_fn_name:ident
        {
            $(
                $ffi_name:ident => fn
                    $fn_name:ident(&self, $($arg:ident : $mut:tt $ty:ty,)*) -> ($ret:ty)
                        $terminator:tt
            )+
        }
    ) => {
        || Communicator {
            trait_name: Ident::new(stringify!($trait_name), Span::call_site()),
            did_you_call_fn_name: Ident::new(stringify!($did_you_call_fn_name), Span::call_site()),
            funcs: vec![$(
                Function {
                    ffi_name: Ident::new(stringify!($ffi_name), Span::call_site()),
                    fn_name: Ident::new(stringify!($fn_name), Span::call_site()),
                    ret: quote!($ret),
                    args: vec![$(
                        Argument {
                            name: Ident::new(stringify!($arg), Span::call_site()),
                            is_mut: communicator!(@is_mut $mut),
                            ty: quote!($ty),
                        }
                    ),*],
                    terminator: quote!($terminator),
                }
            ),+],
        }
    };
}

static SERVER_CONFIG: fn() -> Communicator = communicator! {
    communicator ServerConfig
        DID_YOU_CALL_server_config_implement_guest_server_MACRO
    {
        server_config_welcome_banner_reply => fn welcome_banner_reply(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply) ;

        server_config_filter_hello => fn filter_hello(
            &self,
            is_ehlo: () bool,
            hostname: () smtp_message::Hostname,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_server_types::SerializableDecision<smtp_server_types::HelloInfo>) ;

        server_config_can_do_tls => fn can_do_tls(
            &self,
            conn_meta: () smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (bool)
        {
            !conn_meta.is_encrypted &&
                conn_meta.hello.as_ref().map(|h| h.is_ehlo).unwrap_or(false)
        }

        server_config_new_mail => fn new_mail(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (Vec<u8>) ;

        server_config_filter_from => fn filter_from(
            &self,
            from: () Option<smtp_message::Email>,
            meta: (&mut) smtp_server_types::MailMetadata<Vec<u8>>,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_server_types::SerializableDecision<Option<smtp_message::Email>>) ;

        server_config_filter_to => fn filter_to(
            &self,
            to: () smtp_message::Email,
            meta: (&mut) smtp_server_types::MailMetadata<Vec<u8>>,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_server_types::SerializableDecision<smtp_message::Email>) ;

        server_config_filter_data => fn filter_data(
            &self,
            meta: (&mut) smtp_server_types::MailMetadata<Vec<u8>>,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_server_types::SerializableDecision<()>)
        {
            smtp_server_types::SerializableDecision::Accept {
                reply: smtp_server_types::reply::okay_data().convert(),
                res: (),
            }
        }

        server_config_handle_rset => fn handle_rset(
            &self,
            meta: (&mut) Option<smtp_server_types::MailMetadata<Vec<u8>>>,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_server_types::SerializableDecision<()>)
        {
            smtp_server_types::SerializableDecision::Accept {
                reply: smtp_server_types::reply::okay_rset().convert(),
                res: (),
            }
        }

        server_config_handle_starttls => fn handle_starttls(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_server_types::SerializableDecision<()>)
        {
            if Self::can_do_tls(cfg, (*conn_meta).clone()) {
                smtp_server_types::SerializableDecision::Accept {
                    reply: smtp_server_types::reply::okay_starttls().convert(),
                    res: (),
                }
            } else {
                smtp_server_types::SerializableDecision::Reject {
                    reply: smtp_server_types::reply::command_not_supported().convert(),
                }
            }
        }

        server_config_handle_expn => fn handle_expn(
            &self,
            name: () smtp_message::MaybeUtf8<String>,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_server_types::SerializableDecision<()>)
        {
            smtp_server_types::SerializableDecision::Reject {
                reply: smtp_server_types::reply::command_unimplemented().convert(),
            }
        }

        server_config_handle_vrfy => fn handle_vrfy(
            &self,
            name: () smtp_message::MaybeUtf8<String>,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_server_types::SerializableDecision<()>)
        {
            smtp_server_types::SerializableDecision::Accept {
                reply: smtp_server_types::reply::ignore_vrfy().convert(),
                res: (),
            }
        }

        server_config_handle_help => fn handle_help(
            &self,
            subject: () smtp_message::MaybeUtf8<String>,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_server_types::SerializableDecision<()>)
        {
            smtp_server_types::SerializableDecision::Accept {
                reply: smtp_server_types::reply::ignore_help().convert(),
                res: (),
            }
        }

        server_config_handle_noop => fn handle_noop(
            &self,
            string: () smtp_message::MaybeUtf8<String>,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_server_types::SerializableDecision<()>)
        {
            smtp_server_types::SerializableDecision::Accept {
                reply: smtp_server_types::reply::okay_noop().convert(),
                res: (),
            }
        }

        server_config_handle_quit => fn handle_quit(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_server_types::SerializableDecision<()>)
        {
            smtp_server_types::SerializableDecision::Kill {
                reply: Some(smtp_server_types::reply::okay_quit().convert()),
                res: Ok(()),
            }
        }

        server_config_already_did_hello => fn already_did_hello(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply)
        {
            smtp_server_types::reply::bad_sequence().convert()
        }

        server_config_mail_before_hello => fn mail_before_hello(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply)
        {
            smtp_server_types::reply::bad_sequence().convert()
        }

        server_config_already_in_mail => fn already_in_mail(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply)
        {
            smtp_server_types::reply::bad_sequence().convert()
        }

        server_config_rcpt_before_mail => fn rcpt_before_mail(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply)
        {
            smtp_server_types::reply::bad_sequence().convert()
        }

        server_config_data_before_rcpt => fn data_before_rcpt(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply)
        {
            smtp_server_types::reply::bad_sequence().convert()
        }

        server_config_data_before_mail => fn data_before_mail(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply)
        {
            smtp_server_types::reply::bad_sequence().convert()
        }

        server_config_starttls_unsupported => fn starttls_unsupported(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply)
        {
            smtp_server_types::reply::command_not_supported().convert()
        }

        server_config_command_unrecognized => fn command_unrecognized(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply)
        {
            smtp_server_types::reply::command_unrecognized().convert()
        }

        server_config_pipeline_forbidden_after_starttls => fn pipeline_forbidden_after_starttls(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply)
        {
            smtp_server_types::reply::pipeline_forbidden_after_starttls().convert()
        }

        server_config_line_too_long => fn line_too_long(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply)
        {
            smtp_server_types::reply::line_too_long().convert()
        }

        server_config_handle_mail_did_not_call_complete => fn handle_mail_did_not_call_complete(
            &self,
            conn_meta: (&mut) smtp_server_types::ConnectionMetadata<Vec<u8>>,
        ) -> (smtp_message::Reply)
        {
            smtp_server_types::reply::handle_mail_did_not_call_complete().convert()
        }

        server_config_reply_write_timeout_in_millis => fn reply_write_timeout_in_millis(
            &self,
        ) -> (u64)
        {
            // 5 minutes in milliseconds
            5 * 60 * 1000
        }

        server_config_command_read_timeout_in_millis => fn command_read_timeout_in_millis(
            &self,
        ) -> (u64)
        {
            // 5 minutes in milliseconds
            5 * 60 * 1000
        }
    }
};

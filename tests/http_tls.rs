extern crate solicit_fork as solicit;
extern crate futures;
extern crate tokio_core;
extern crate tokio_tls;
extern crate grpc;

mod test_misc;


macro_rules! t {
    ($e:expr) => (match $e {
        Ok(e) => e,
        Err(e) => panic!("{} failed with {:?}", stringify!($e), e),
    })
}


#[cfg(feature = "openssl")]
mod test_with_openssl {
    extern crate openssl;

    use grpc::for_test::*;

    use test_misc::*;

    use std::io;
    use std::fs::File;
    use std::env;
    use std::sync::{Once, ONCE_INIT};
    use std::thread;
    use std::sync::mpsc;
    use std::net::ToSocketAddrs;

    use futures;
    use futures::Future;
    use futures::stream;
    use futures::stream::Stream;

    use solicit::http::Header;
    use solicit::http::HttpError;

    use self::openssl::crypto::hash::Type;
    use self::openssl::crypto::pkey::PKey;
    use self::openssl::crypto::rsa::RSA;
    use self::openssl::x509::{X509Generator, X509};

    use tokio_core::reactor;

    use tokio_tls::backend::openssl::ServerContextExt;
    use tokio_tls::backend::openssl::ClientContextExt;
    use tokio_tls::ServerContext;

    // copy-paste from tokio-tls
    // https://github.com/tokio-rs/tokio-tls/issues/6

    fn server_cx() -> io::Result<ServerContext> {
        let (cert, key) = keys();
        ServerContext::new(cert, key)
    }

    /*
    fn configure_client(cx: &mut ClientContext) {
        // Unfortunately looks like the only way to configure this is
        // `set_CA_file` file on the client side so we have to actually
        // emit the certificate to a file. Do so next to our own binary
        // which is likely ephemeral as well.
        let path = t!(env::current_exe());
        let path = path.parent().unwrap().join("custom.crt");
        static INIT: Once = ONCE_INIT;
        INIT.call_once(|| {
            let pem = keys().0.to_pem().unwrap();
            t!(t!(File::create(&path)).write_all(&pem));
        });
        let ssl = cx.ssl_context_mut();
        t!(ssl.set_CA_file(&path));
    }
    */

    // Generates a new key on the fly to be used for the entire suite of
    // tests here.
    fn keys() -> (&'static X509, &'static PKey) {
        static INIT: Once = ONCE_INIT;
        static mut CERT: *mut X509 = 0 as *mut _;
        static mut KEY: *mut PKey = 0 as *mut _;

        unsafe {
            INIT.call_once(|| {
                let rsa = RSA::generate(1024).unwrap();
                let pkey = PKey::from_rsa(rsa).unwrap();
                let gen = X509Generator::new()
                            .set_valid_period(1)
                            .add_name("CN".to_owned(), "localhost".to_owned())
                            .set_sign_hash(Type::SHA256);
                let cert = gen.sign(&pkey).unwrap();

                CERT = Box::into_raw(Box::new(cert));
                KEY = Box::into_raw(Box::new(pkey));
            });

            (&*CERT, &*KEY)
        }
    }

    #[test]
    fn test() {
        let server_cx = server_cx().unwrap();
        let server = HttpServerOneConn::new_tls_fn(0, server_cx, |_headers, req| {
            Box::new(future_flatten_to_stream(req
                .fold(Vec::new(), |mut v, message| {
                    match message.content {
                        HttpStreamPartContent::Headers(..) => (),
                        HttpStreamPartContent::Data(d) => v.extend(d),
                    }

                    futures::finished::<_, HttpError>(v)
                })
                .and_then(|v| {
                    let mut r = Vec::new();
                    r.push(HttpStreamPart::intermediate_headers(
                        vec![
                            Header::new(":status", "200"),
                        ]
                    ));
                    r.push(HttpStreamPart::last_data(v));
                    Ok(stream::iter(r.into_iter().map(Ok)))
                })))
        });

        let port = server.port();

        let (client_complete_tx, client_complete_rx) = mpsc::channel();

        thread::spawn(move || {
            let mut client_lp = reactor::Core::new().expect("core");

            let (client, future) = HttpClientConnectionAsync::new_tls(client_lp.handle(), &("::1", port).to_socket_addrs().unwrap().next().unwrap());

            let resp = client.start_request(
                Vec::new(),
                Box::new(stream_once((&b"abcd"[..]).to_owned())));

            let request_future = resp.fold(Vec::new(), move |mut v, part| {
                match part.content {
                    HttpStreamPartContent::Headers(..) => (),
                    HttpStreamPartContent::Data(data) => v.extend(data),
                }
                if part.last {
                    client_complete_tx.send(v.clone()).unwrap()
                }
                futures::finished::<_, HttpError>(v)
            }).map(|_| ());

            client_lp.run(future.select(request_future)).ok();
        });

        assert_eq!(&b"abcd"[..], &client_complete_rx.recv().expect("client complete recv")[..]);
    }
}

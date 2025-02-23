use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::rc::{Rc, Weak};

use gloo_utils::window;
use js_sys::Array;
use web_sys::{Blob, BlobPropertyBag, Url};

use crate::bridge::{CallbackMap, WorkerBridge};
use crate::codec::{Bincode, Codec};
use crate::handler_id::HandlerId;
use crate::messages::FromWorker;
use crate::native_worker::{DedicatedWorker, NativeWorkerExt};
use crate::traits::Worker;
use crate::{Callback, Shared};

fn create_worker(path: &str) -> DedicatedWorker {
    let js_shim_url = Url::new_with_base(
        path,
        &window().location().href().expect("failed to read href."),
    )
    .expect("failed to create url for javascript entrypoint")
    .to_string();

    let wasm_url = js_shim_url.replace(".js", "_bg.wasm");

    let array = Array::new();
    array.push(&format!(r#"importScripts("{js_shim_url}");wasm_bindgen("{wasm_url}");"#).into());
    let blob = Blob::new_with_str_sequence_and_options(
        &array,
        BlobPropertyBag::new().type_("application/javascript"),
    )
    .unwrap();
    let url = Url::create_object_url_with_blob(&blob).unwrap();

    DedicatedWorker::new(&url).expect("failed to spawn worker")
}

/// A spawner to create workers.
#[derive(Clone)]
pub struct WorkerSpawner<W, CODEC = Bincode>
where
    W: Worker,
    CODEC: Codec,
{
    _marker: PhantomData<(W, CODEC)>,
    callback: Option<Callback<W::Output>>,
}

impl<W, CODEC> fmt::Debug for WorkerSpawner<W, CODEC>
where
    W: Worker,
    CODEC: Codec,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WorkerScope<_>")
    }
}

impl<W, CODEC> Default for WorkerSpawner<W, CODEC>
where
    W: Worker,
    CODEC: Codec,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<W, CODEC> WorkerSpawner<W, CODEC>
where
    W: Worker,
    CODEC: Codec,
{
    /// Creates a [WorkerSpawner].
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
            callback: None,
        }
    }

    /// Sets a new message encoding.
    pub fn encoding<C>(&mut self) -> WorkerSpawner<W, C>
    where
        C: Codec,
    {
        WorkerSpawner {
            _marker: PhantomData,
            callback: self.callback.clone(),
        }
    }

    /// Sets a callback.
    pub fn callback<F>(&mut self, cb: F) -> &mut Self
    where
        F: 'static + Fn(W::Output),
    {
        self.callback = Some(Rc::new(cb));

        self
    }

    /// Spawns a Worker.
    pub fn spawn(&self, path: &str) -> WorkerBridge<W> {
        let pending_queue = Rc::new(RefCell::new(Some(Vec::new())));
        let handler_id = HandlerId::new();
        let mut callbacks = HashMap::new();

        if let Some(m) = self.callback.as_ref().map(Rc::downgrade) {
            callbacks.insert(handler_id, m);
        }

        let callbacks: Shared<CallbackMap<W>> = Rc::new(RefCell::new(callbacks));

        let worker = {
            let pending_queue = pending_queue.clone();
            let callbacks = callbacks.clone();
            let worker = create_worker(path);

            let handler = {
                let worker = worker.clone();

                move |msg: FromWorker<W>| match msg {
                    FromWorker::WorkerLoaded => {
                        if let Some(pending_queue) = pending_queue.borrow_mut().take() {
                            for to_worker in pending_queue.into_iter() {
                                worker.post_packed_message::<_, CODEC>(to_worker);
                            }
                        }
                    }
                    FromWorker::ProcessOutput(id, output) => {
                        let mut callbacks = callbacks.borrow_mut();

                        if let Some(m) = callbacks.get(&id) {
                            if let Some(m) = Weak::upgrade(m) {
                                m(output);
                            } else {
                                callbacks.remove(&id);
                            }
                        }
                    }
                }
            };

            worker.set_on_packed_message::<_, CODEC, _>(handler);
            worker
        };

        WorkerBridge::<W>::new::<CODEC>(
            handler_id,
            worker,
            pending_queue,
            callbacks,
            self.callback.clone(),
        )
    }
}

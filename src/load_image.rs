use std::cell::RefCell;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{window, Blob, FileReader, HtmlInputElement};


thread_local! {
    // Bytes read from the most recently picked image, waiting to be picked up
    // by the main redraw loop.
    static PENDING_BYTES: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };

    // Persistent hidden <input type="file">; created lazily on first pick.
    static FILE_INPUT: RefCell<Option<HtmlInputElement>> = const { RefCell::new(None) };

    // Holds the `change` and `load` closures so they aren't dropped while
    // the browser still holds a reference.
    static CHANGE_CLOSURE: RefCell<Option<Closure<dyn FnMut(web_sys::Event)>>> = const { RefCell::new(None) };
    static LOAD_CLOSURE:   RefCell<Option<Closure<dyn FnMut(web_sys::Event)>>> = const { RefCell::new(None) };
    static ERROR_CLOSURE:  RefCell<Option<Closure<dyn FnMut(web_sys::Event)>>> = const { RefCell::new(None) };
}


fn ensure_input() -> Result<HtmlInputElement, wasm_bindgen::JsValue> {
    if let Some(input) = FILE_INPUT.with(|c| c.borrow().clone()) {
        return Ok(input);
    }

    let document = window().unwrap().document().unwrap();

    let input = document
        .create_element("input")?
        .dyn_into::<HtmlInputElement>()?;
    input.set_type("file");
    input.set_accept("image/*");
    input.style().set_property("display", "none")?;
    document.body().unwrap().append_child(&input)?;

    FILE_INPUT.with(|c| *c.borrow_mut() = Some(input.clone()));
    Ok(input)
}


fn install_change_listener(input: &HtmlInputElement) {
    // Drop any previous listener closures before installing new ones, so a
    // second call to `pick` replaces the handler cleanly.
    CHANGE_CLOSURE.with(|c| { let _ = c.borrow_mut().take(); });
    LOAD_CLOSURE.with(|c|   { let _ = c.borrow_mut().take(); });
    ERROR_CLOSURE.with(|c|  { let _ = c.borrow_mut().take(); });

    let onchange: Closure<dyn FnMut(web_sys::Event)> = Closure::new(move |evt: web_sys::Event| {
        let target = match evt.target() {
            Some(t) => t,
            None => return,
        };
        let input: HtmlInputElement = match target.dyn_into::<HtmlInputElement>() {
            Ok(i) => i,
            Err(_) => return,
        };

        let files = match input.files() {
            Some(f) => f,
            None => return,
        };
        if files.length() == 0 { return; }

        let file: web_sys::File = match files.get(0) {
            Some(f) => f,
            None => return,
        };

        // Reset the input's value so picking the same file twice still fires
        // `change`.
        input.set_value("");

        // Build the per-read closures inside this body so they can be stored
        // in the thread-locals without conflicting with the `'static` borrow
        // requirements of `onchange`'s capture set.
        let onload: Closure<dyn FnMut(web_sys::Event)> = Closure::new(|evt: web_sys::Event| {
            let reader: FileReader = match evt.target() {
                Some(t) => match t.dyn_into::<FileReader>() {
                    Ok(r) => r,
                    Err(_) => return,
                },
                None => return,
            };

            let result = match reader.result() {
                Ok(v) => v,
                Err(_) => return,
            };

            // `result` is an ArrayBuffer.
            let buffer: js_sys::ArrayBuffer = result.unchecked_into();
            let view = js_sys::Uint8Array::new(&buffer);
            let mut bytes = vec![0u8; view.length() as usize];
            view.copy_to(&mut bytes);
            PENDING_BYTES.with(|c| *c.borrow_mut() = Some(bytes));
        });

        let onerror: Closure<dyn FnMut(web_sys::Event)> = Closure::new(|_evt: web_sys::Event| {
            web_sys::console::error_1(&"[molasses] file reader error".into());
        });

        let reader = FileReader::new().unwrap();
        reader.set_onload(Some(onload.as_ref().unchecked_ref()));
        reader.set_onerror(Some(onerror.as_ref().unchecked_ref()));

        // Keep the closures alive until the read fires.
        LOAD_CLOSURE.with(|c| *c.borrow_mut() = Some(onload));
        ERROR_CLOSURE.with(|c| *c.borrow_mut() = Some(onerror));

        let _ = reader.read_as_array_buffer(&file.dyn_into::<Blob>().unwrap());
    });

    input.set_onchange(Some(onchange.as_ref().unchecked_ref()));

    // Keep the change listener alive for as long as the input element exists.
    CHANGE_CLOSURE.with(|c| *c.borrow_mut() = Some(onchange));
}


pub fn pick() {
    match ensure_input() {
        Ok(input) => {
            install_change_listener(&input);
            input.click();
        }
        Err(e) => web_sys::console::error_1(&e),
    }
}


pub fn take_pending() -> Option<Vec<u8>> {
    PENDING_BYTES.with(|c| c.borrow_mut().take())
}

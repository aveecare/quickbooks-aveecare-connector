//! The real thing: Intuit's QuickBooks Desktop SDK request processor,
//! `QBXMLRP2.RequestProcessor`, over COM.
//!
//! The same five calls Intuit's Web Connector makes:
//!
//!   OpenConnection2("", appName, localQBD)    // register as an integrated app
//!   BeginSession("", qbFileOpenDoNotCare)     // attach to the file QuickBooks has open
//!   ProcessRequest(ticket, qbXML) -> qbXML    // once per request
//!   EndSession(ticket)
//!   CloseConnection()
//!
//! Called by name through IDispatch (late binding), exactly as PowerShell and
//! the Web Connector's own .NET interop do, so no type library is needed at
//! build time and any installed SDK version answers.
//!
//! COM objects belong to the thread that made them: create, use and drop a
//! `ComTransport` on one thread (the sync thread does).

use super::{classify_hresult, OpenedFile, QbError, QbErrorKind, QbTransport};
use std::mem::ManuallyDrop;
use windows::core::{BSTR, GUID, HSTRING, PCWSTR};
use windows::Win32::System::Com::{
    CLSIDFromProgID, CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch, CLSCTX_ALL, COINIT_APARTMENTTHREADED, DISPATCH_METHOD,
    DISPPARAMS, EXCEPINFO,
};
use windows::Win32::System::Variant::{VariantClear, VARIANT, VT_BSTR, VT_I4};

/// One argument to a COM call.
enum Arg<'a> {
    Str(&'a str),
    Int(i32),
}

fn to_variant(a: &Arg) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        match a {
            Arg::Str(s) => {
                inner.vt = VT_BSTR;
                inner.Anonymous.bstrVal = ManuallyDrop::new(BSTR::from(*s));
            }
            Arg::Int(i) => {
                inner.vt = VT_I4;
                inner.Anonymous.lVal = *i;
            }
        }
    }
    v
}

/// The string in a result, then release it.
fn take_string(mut v: VARIANT) -> String {
    let s = unsafe {
        let inner = &*v.Anonymous.Anonymous;
        if inner.vt == VT_BSTR {
            inner.Anonymous.bstrVal.to_string()
        } else {
            String::new()
        }
    };
    unsafe {
        let _ = VariantClear(&mut v);
    }
    s
}

const PROG_ID: &str = "QBXMLRP2.RequestProcessor";
/// QBXMLRPConnectionType.localQBD
const LOCAL_QBD: i32 = 1;
/// QBFileMode.qbFileOpenDoNotCare: whatever mode QuickBooks has the file in.
const FILE_MODE_DO_NOT_CARE: i32 = 2;
const DISP_E_EXCEPTION: i32 = 0x80020009_u32 as i32;

pub struct ComTransport {
    rp: Option<IDispatch>,
    ticket: Option<String>,
    connected: bool,
    com_initialized: bool,
}

impl ComTransport {
    pub fn new() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        Self { rp: None, ticket: None, connected: false, com_initialized: hr.is_ok() }
    }

    fn rp(&self) -> Result<&IDispatch, QbError> {
        self.rp.as_ref().ok_or_else(|| QbError::new(QbErrorKind::Other, "Not connected to QuickBooks.", "Sync again."))
    }

    /// Call a method by name with BSTR/int arguments, in natural order.
    fn call(&self, name: &str, args: &[Arg]) -> Result<VARIANT, QbError> {
        let rp = self.rp()?;
        unsafe {
            let wide = HSTRING::from(name);
            let names = [PCWSTR(wide.as_ptr())];
            let mut dispid = 0i32;
            rp.GetIDsOfNames(&GUID::zeroed(), names.as_ptr(), 1, 0x0400, &mut dispid)
                .map_err(|e| classify_hresult(e.code().0 as u32, &e.message()))?;

            // IDispatch takes its arguments last-first.
            let mut reversed: Vec<VARIANT> = args.iter().rev().map(to_variant).collect();
            let params = DISPPARAMS {
                rgvarg: if reversed.is_empty() { std::ptr::null_mut() } else { reversed.as_mut_ptr() },
                rgdispidNamedArgs: std::ptr::null_mut(),
                cArgs: reversed.len() as u32,
                cNamedArgs: 0,
            };
            let mut result = VARIANT::default();
            let mut excep = EXCEPINFO::default();
            let mut arg_err = 0u32;
            let r = rp.Invoke(
                dispid,
                &GUID::zeroed(),
                0x0400,
                DISPATCH_METHOD,
                &params,
                Some(&mut result),
                Some(&mut excep),
                Some(&mut arg_err),
            );
            for v in reversed.iter_mut() {
                let _ = VariantClear(v);
            }
            let source = ManuallyDrop::take(&mut excep.bstrSource);
            let description = ManuallyDrop::take(&mut excep.bstrDescription);
            let _help = ManuallyDrop::take(&mut excep.bstrHelpFile);
            drop(source);
            match r {
                Ok(()) => Ok(result),
                Err(e) if e.code().0 == DISP_E_EXCEPTION => {
                    // QBXMLRP2 reports its own HRESULT (0x80040408 and friends)
                    // inside the exception, with its own sentence.
                    let scode = excep.scode as u32;
                    let code = if scode != 0 { scode } else { e.code().0 as u32 };
                    Err(classify_hresult(code, &description.to_string()))
                }
                Err(e) => Err(classify_hresult(e.code().0 as u32, &e.message())),
            }
        }
    }

    fn bstr_of(v: VARIANT) -> String {
        take_string(v)
    }
}

impl Default for ComTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl QbTransport for ComTransport {
    fn open(&mut self, app_id: &str, app_name: &str) -> Result<OpenedFile, QbError> {
        unsafe {
            let prog = HSTRING::from(PROG_ID);
            let clsid = CLSIDFromProgID(PCWSTR(prog.as_ptr())).map_err(|_| classify_hresult(0x800401F3, "QBXMLRP2 is not registered."))?;
            let rp: IDispatch = CoCreateInstance(&clsid, None, CLSCTX_ALL).map_err(|e| classify_hresult(e.code().0 as u32, &e.message()))?;
            self.rp = Some(rp);
        }
        self.call("OpenConnection2", &[Arg::Str(app_id), Arg::Str(app_name), Arg::Int(LOCAL_QBD)]).map(take_string)?;
        self.connected = true;
        let ticket = Self::bstr_of(self.call("BeginSession", &[Arg::Str(""), Arg::Int(FILE_MODE_DO_NOT_CARE)])?);
        if ticket.is_empty() {
            return Err(QbError::new(QbErrorKind::NoCompanyOpen, "QuickBooks has no company file open.", "Open your company file in QuickBooks, then sync again."));
        }
        self.ticket = Some(ticket.clone());
        let company_file = self
            .call("GetCurrentCompanyFileName", &[Arg::Str(&ticket)])
            .map(Self::bstr_of)
            .unwrap_or_default();
        Ok(OpenedFile { company_file })
    }

    fn process(&mut self, request: &str) -> Result<String, QbError> {
        let ticket = self.ticket.clone().ok_or_else(|| QbError::new(QbErrorKind::Other, "Not connected to QuickBooks.", "Sync again."))?;
        let v = self.call("ProcessRequest", &[Arg::Str(&ticket), Arg::Str(request)])?;
        Ok(Self::bstr_of(v))
    }

    fn close(&mut self) {
        if let Some(t) = self.ticket.take() {
            let _ = self.call("EndSession", &[Arg::Str(&t)]).map(take_string);
        }
        if self.connected {
            let _ = self.call("CloseConnection", &[]).map(take_string);
            self.connected = false;
        }
        self.rp = None;
    }
}

impl Drop for ComTransport {
    fn drop(&mut self) {
        self.close();
        if self.com_initialized {
            unsafe { CoUninitialize() };
        }
    }
}

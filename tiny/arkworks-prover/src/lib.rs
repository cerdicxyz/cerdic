// Library surface so other tiny/ crates (the TEE, the client) can call the
// note-derivation logic directly as a normal function — no subprocess, no
// second implementation in another language. This is the whole reason for
// this file existing: the TEE briefly had its own TypeScript reimplementation
// of MiMC-5, which is exactly the kind of drift risk a shared library removes.
pub mod circuit;
pub mod mimc;
pub mod note_circuit;

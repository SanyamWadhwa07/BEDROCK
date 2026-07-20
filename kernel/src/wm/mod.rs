pub mod window;
pub mod zorder;

// `wm::wm` reads oddly, but the alternative is renaming the compositor
// module to something less accurate than what it is. The task entry point
// is re-exported below so callers say `wm::wm_task`.
#[allow(clippy::module_inception)]
pub mod wm;

pub use wm::wm_task;

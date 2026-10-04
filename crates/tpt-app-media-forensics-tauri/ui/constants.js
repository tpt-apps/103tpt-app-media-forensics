// Values shared between the Rust side and this frontend.
//
// The progress event name lives here rather than being written twice. It is
// emitted by `commands::PROGRESS_EVENT` and listened for here, and a rename on
// one side only would fail silently as a progress bar that never moves - which
// reads as an analysis that has hung.
export const PROGRESS_EVENT = "analysis-progress";

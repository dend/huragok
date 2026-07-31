//! Confirmed RVAs and struct offsets for the patched *Meteorite* build (2026-07-29).
//!
//! Code RVAs were re-anchored after the game update by disassembling the shipping exe
//! (string anchors, byte-pattern sig scans, xref math). Struct offsets are engine
//! layout (UE 5.5) and generally survive content patches - but see the SUSPECT notes.
//!
//! Ported from the C++ reference (see `reference/HCECamera.cpp`).

// ---------------- data globals (module base + RVA) ----------------
pub const GUOBJECTARRAY: usize = 0x0d0b_1770; // was 0x0d0b8770; .data shifted -0x7000
pub const FNAMEPOOL: usize = 0x0d40_ed80; //     was 0x0d415d80; same -0x7000 shift
pub const GAVERAGEFPS: usize = 0x0d55_0f94; // UE's GAverageFPS global (the `%5.2f FPS` value)

// ---------------- native functions (module base + RVA) ----------------
pub const PROCESSEVENT_SLOT: usize = 79; // UObject::ProcessEvent vtable slot (index, patch-proof)
pub const GETFOVANGLE_SLOT: usize = 252; // camera manager vtable slot (byte 0x7E0): GetFOVAngle

pub const SET_CAMERA_PERSPECTIVE: usize = 0x07b1_6da0; // void(pawn, u8 persp, i32* ctx)
pub const REP_UPDATER: usize = 0x07b1_6de0; //           void(pawn, i32 ctx0, i32 ctx1)
pub const GET_REP_BY_INDEX: usize = 0x07b1_6d30; //      *rep(pawn, i32 idx) - TMap lookup
pub const SET_ACTIVE_REP: usize = 0x07b1_5b90; //        void(pawn, i32 idx) - death-path activator

// Dear ImGui native entry points (harvested from the demo / DrawControls).
pub const IMGUI_BEGIN: usize = 0x072c_1530;
pub const IMGUI_END: usize = 0x072c_52d0;
pub const IMGUI_TEXT: usize = 0x0731_04c0;
pub const IMGUI_BUTTON: usize = 0x0731_1a70; // Button(label, &size) -> ButtonEx
pub const IMGUI_TREENODE: usize = 0x0732_5690; // TreeNodeBehavior(id, flags, label, label_end)
pub const IMGUI_CHECKBOX: usize = 0x0731_3050; // Checkbox(label, bool*) -> bool
pub const IMGUI_SLIDER_FLOAT: usize = 0x0731_91f0; // SliderFloat(label, f32*, min, max, fmt, flags) -> bool
pub const IMGUI_PROGRESS_BAR: usize = 0x0731_3ad0; // ProgressBar(fraction, ImVec2*, overlay)
pub const IMGUI_SEPARATOR: usize = 0x0732_95d0; // Separator()
pub const IMGUI_INVISIBLE_BUTTON: usize = 0x0731_1ae0; // InvisibleButton(id, ImVec2*, flags) -> bool
pub const IMGUI_DRAW_ADD_LINE: usize = 0x0730_79e0; // ImDrawList::AddLine(this, p1*, p2*, col, thick)
pub const IMGUI_DRAW_ADD_RECT_FILLED: usize = 0x0730_7c20; // AddRectFilled(this, min*, max*, col, round, flags)
pub const IMGUI_DRAW_ADD_CIRCLE_FILLED: usize = 0x0730_8180; // AddCircleFilled(this, center*, r, col, segs)
pub const IMGUI_INPUT_TEXT: usize = 0x0731_b270; // InputText(label, buf, buf_size, flags, cb, ud) -> bool
pub const IMGUI_BEGIN_CHILD: usize = 0x0072_bfbe0; // BeginChild(str_id, &size, border(0/1), flags) -> bool
pub const IMGUI_END_CHILD: usize = 0x0072_bfc40; // EndChild()
pub const IMGUI_SET_SCROLL_HERE_Y: usize = 0x0072_cb3a0; // SetScrollHereY(ratio) - pass 1.0 for bottom
// ImGuiWindow field offsets (from the current-window pointer at ctx+0x3ed8).
pub const IMGUI_WIN_SCROLL_Y: usize = 0x88; // window.Scroll.y
pub const IMGUI_WIN_SCROLLMAX_Y: usize = 0x90; // window.ScrollMax.y
pub const IMGUI_WIN_CONTENT_MAX_X: usize = 0x250; // window.ContentRegionRect.Max.x
// Inlined ImGui accessors (no exported fn): resolve via these offsets.
pub const GIMGUI_PTR: usize = 0x0d56_a008; // *ImGuiContext global (verified via RIP-relative loads)
pub const IMGUI_CTX_CURRENT_WINDOW: usize = 0x3ed8; // ImGuiContext.CurrentWindow
pub const IMGUI_WIN_DRAWLIST: usize = 0x0298; // ImGuiWindow.DrawList
pub const IMGUI_WIN_CURSOR_POS: usize = 0x0100; // ImGuiWindow.DC.CursorPos (screen)
pub const DRAWCONTROLS: usize = 0x0739_d270; // FImGuiDemo::DrawControls (per-frame draw cb)
pub const DRAWCONTROLS_SLOT: usize = 0x0bf4_9950; // .rdata dispatch pointer we swap

// ---------------- UObject / reflection layout ----------------
pub const ELEMENTS_PER_CHUNK: usize = 64 * 1024;
pub const ITEM_STRIDE: usize = 0x18; // FUObjectItem stride
pub const UOA_OBJECTS: usize = 0x10; // FUObjectArray.Objects  (chunk pointer array)
pub const UOA_NUMELEMENTS: usize = 0x24;
pub const UOA_NUMCHUNKS: usize = 0x2c;
pub const UO_FLAGS: usize = 0x08;
pub const UO_CLASS: usize = 0x10;
pub const UO_NAME: usize = 0x18;
pub const UO_OUTER: usize = 0x20;
pub const UF_NEXT: usize = 0x28; // FField.Next / linked children
pub const US_SUPER: usize = 0x40; // UStruct.SuperStruct
pub const US_CHILDREN: usize = 0x48; // UStruct.Children
pub const UFN_PARMSSIZE: usize = 0xb6; // UFunction.ParmsSize (u16)
pub const NP_BLOCKS: usize = 0x10; // FNamePool.Blocks
pub const NAME_BLOCK_BITS: u32 = 16;

pub const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;
pub const RF_ARCHETYPE_OBJECT: u32 = 0x20;
pub const RF_BIT30: u32 = 0x4000_0000; // ProcessEvent refuses objects with this set

// ---------------- pawn / world-representation (engine layout - verify at runtime) ----------------
pub const PAWN_PERSPECTIVE: usize = 0x3c1; // u8: 1 = first-person, 2 = third-person
pub const PAWN_ACTIVE_REP: usize = 0x3f8; // i32 active-rep index - SUSPECT: read garbage post-patch
pub const PAWN_REPMGR: usize = 0x428; // rep-manager pointer (looked valid post-patch)
pub const REPMGR_ACTIVE_WORLD_REP: usize = 0x188; // i32, -1 = none active
pub const REPMGR_GATE_SHOW: usize = 0x13c; // u8 world-body show gate
pub const REPMGR_GATE_HIDEFP: usize = 0xc0; // u8 hide-first-person-arms gate

// ---------------- BlueprintUpdateCamera parms ----------------
pub const BUC_LOCATION: usize = 0x08; // FVector  (3 x f64)
pub const BUC_ROTATION: usize = 0x20; // FRotator (pitch, yaw, roll f64)
pub const BUC_FOV: usize = 0x38; // f32
pub const BUC_RETURN: usize = 0x3c; // bool

// ---------------- camera manager (APlayerCameraManager subclass) ----------------
pub const CAMMGR_POV_FOV: usize = 0x3b0; // ViewTarget.POV.FOV (f32) - write to force FOV
pub const CAMMGR_POV_DESIRED_FOV: usize = 0x3b4; // ViewTarget.POV.DesiredFOV (f32)

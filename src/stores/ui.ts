import { create } from "zustand";

interface UiStore {
  /** 悬浮模式（false）还是完整模式（true）。 */
  expanded: boolean;
  settingsOpen: boolean;
  /** 完整模式下是否隐藏历史侧栏。 */
  sidebarHidden: boolean;
  /** 入场 / 退场动画阶段。 */
  phase: "hidden" | "entering" | "shown" | "leaving";
  /** 快捷键注册失败等信息。 */
  notices: string[];
  /**
   * 在这个时间点之前忽略窗口失焦事件。
   *
   * 程序化 `show()` 之后系统会立刻送来一次 blur，若不屏蔽，用户按
   * Alt+Space 唤出的窗口会在同一帧被自己的「失焦隐藏」逻辑关掉 ——
   * 表现为「窗口一闪而过」。
   */
  blurGuardUntil: number;
  /**
   * 划词取词（Alt+Q）抓到的文本。Composer 订阅它，非空时把它追加进输入框。
   *
   * 用 store 而不是直接操作 DOM：取词发生在「窗口还没聚焦」的时刻，
   * 而输入框的草稿是 React 本地状态，两者需要一个通道对接。
   */
  injectedInput: string | null;

  setExpanded: (expanded: boolean) => void;
  openSettings: () => void;
  closeSettings: () => void;
  toggleSidebar: () => void;
  setPhase: (phase: UiStore["phase"]) => void;
  guardBlur: (ms: number) => void;
  pushNotice: (message: string) => void;
  setNotices: (messages: string[]) => void;
  dismissNotice: (index: number) => void;
  injectInput: (text: string) => void;
  consumeInjectedInput: () => void;
}

export const useUi = create<UiStore>((set) => ({
  expanded: false,
  settingsOpen: false,
  sidebarHidden: false,
  phase: "hidden",
  notices: [],
  blurGuardUntil: 0,
  injectedInput: null,

  setExpanded: (expanded) => set({ expanded }),
  openSettings: () => set({ settingsOpen: true }),
  closeSettings: () => set({ settingsOpen: false }),
  toggleSidebar: () => set((s) => ({ sidebarHidden: !s.sidebarHidden })),
  setPhase: (phase) => set({ phase }),
  guardBlur: (ms) => set({ blurGuardUntil: Date.now() + ms }),
  pushNotice: (message) => set((s) => ({ notices: [...s.notices, message] })),
  setNotices: (messages) => set({ notices: messages }),
  dismissNotice: (index) =>
    set((s) => ({ notices: s.notices.filter((_, i) => i !== index) })),
  injectInput: (text) => set({ injectedInput: text }),
  consumeInjectedInput: () => set({ injectedInput: null }),
}));

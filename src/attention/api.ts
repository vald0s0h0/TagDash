import { invoke } from "@tauri-apps/api/core";
import type {
  AttentionOptions, AttentionWindowState, Detail, MapReply, ReviewContext,
} from "./types";

/** Events exchanged between the main window and the detached screen. */
export const ATTENTION_WINDOW_EVENT    = "attention://window";
export const ATTENTION_PROPAGATE_EVENT = "attention://propagate";
export const ATTENTION_REVIEW_EVENT    = "attention://review";

export const attentionApi = {
  windowSet:   (enabled: boolean) =>
    invoke<AttentionWindowState>("attention_window_set", { enabled }),
  windowState: () => invoke<AttentionWindowState>("attention_window_state"),
  getOptions:  () => invoke<AttentionOptions>("get_attention_options"),
  setOptions:  (options: AttentionOptions) =>
    invoke<AttentionOptions>("set_attention_options", { options }),
  getMap:      (known: string | null, follow_review: boolean) =>
    invoke<MapReply>("get_attention_map", { known, follow_review }),
  getDetail:   (symbol: string, follow_review: boolean) =>
    invoke<Detail | null>("get_attention_detail", { symbol, follow_review }),
  propagate:   (symbol: string) => invoke<void>("attention_propagate", { symbol }),
  setReview:   (review: ReviewContext | null) =>
    invoke<void>("attention_set_review", { review }),
  snapshotDays: () => invoke<string[]>("get_attention_snapshot_days"),
};

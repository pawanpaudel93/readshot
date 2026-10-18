//! The message lists `update_message` routes to each sub-handler, as
//! pattern macros so the dispatcher stays short while its `match` stays
//! exhaustive: a variant missing from every list is still a compile error.

/// Every message the History handler
/// (`history_ops::handle_history_message`) takes.
macro_rules! history_messages {
    () => {
        (Message::HistoryRecordPersisted(..)
            | Message::OpenHistoryRequested
            | Message::HistoryWindowReady(..)
            | Message::HistoryListLoaded(..)
            | Message::HistoryClosed
            | Message::HistorySearchChanged(..)
            | Message::HistoryStatusDismissed
            | Message::HistorySelect(..)
            | Message::HistorySelectPrevious
            | Message::HistorySelectNext
            | Message::HistoryOpenSelected
            | Message::HistoryDeleteSelected
            | Message::HistoryKeyboardShortcut(..)
            | Message::HistoryCapturedShortcut(..)
            | Message::HistoryClearAllRequested
            | Message::HistoryClearAllCancelled
            | Message::HistoryClearAllConfirmed
            | Message::HistoryOpenInEditor(..)
            | Message::HistoryReveal(..)
            | Message::HistoryCopyImage(..)
            | Message::HistoryCopyImageDone(..)
            | Message::HistoryCopyText(..)
            | Message::HistoryCopyTextDone(..)
            | Message::HistoryCopyVisibleTextRequested
            | Message::HistoryCopyVisibleTextDone(..)
            | Message::HistoryPin(..)
            | Message::HistoryPinReady(..)
            | Message::HistoryDelete(..)
            | Message::HistoryDeleteConfirmed(..)
            | Message::HistoryDeleteCancelled
            | Message::HistoryRecordSaved
            | Message::HistoryRecordSaveFailed { .. }
            | Message::HistoryReopenedAfterSync { .. }
            | Message::HistoryShowMore)
    };
}

/// Every message the editor handler
/// (`editor_ops::handle_editor_message`) takes.
macro_rules! editor_messages {
    () => {
        (Message::EditorKeyPressed { .. }
            | Message::EditorStatusTick
            | Message::EditorWindowReady(..)
            | Message::EditorSaveRequested
            | Message::EditorSaved { .. }
            | Message::EditorCopyImageRequested
            | Message::EditorCopyImageDone { .. }
            | Message::EditorBeautifyPreset(..)
            | Message::EditorBeautifySavedPresetApplied(..)
            | Message::EditorBeautifyEdit(..)
            | Message::EditorBeautifySliderPreview(..)
            | Message::EditorSnapshotSliderCommit
            | Message::EditorBeautifySavePreset
            | Message::EditorBeautifyDeletePreset(..)
            | Message::EditorBeautifyRestorePreset
            | Message::EditorBeautifyHexChanged(..)
            | Message::EditorBeautifyHexSubmitted
            | Message::EditorBeautifyPickBackground
            | Message::EditorCopyTextRequested
            | Message::EditorCopyTextDone { .. }
            | Message::EditorDiscardRequested
            | Message::EditorDiscardDisarmed
            | Message::EditorStatusDismissed
            | Message::EditorPopoverToggled(..)
            | Message::EditorPopoverClosed
            | Message::EditorToolbar(..)
            | Message::EditorLineWidthPreview(..)
            | Message::EditorLineWidthCommit
            | Message::EditorStyle(..)
            | Message::EditorStylePreview(..)
            | Message::EditorStyleCommit
            | Message::EditorHexChanged(..)
            | Message::EditorHexSubmitted
            | Message::EditorEyedropperToggled
            | Message::EditorCanvas(..)
            | Message::EditorEditSelectedText
            | Message::EditorImageMenuKey
            | Message::EditorPanelTabKey(..)
            | Message::EditorFocusZoneCycle(..)
            | Message::EditorCycleSelection(..)
            | Message::EditorTabResolved { .. }
            | Message::EditorFieldKeyResolved { .. }
            | Message::EditorResizeByKeys(..)
            | Message::EditorDeleteSelected
            | Message::EditorCropApply
            | Message::EditorCropCancel
            | Message::EditorNudgeSelected(..)
            | Message::EditorTextChanged(..)
            | Message::EditorTextAction(..)
            | Message::EditorTextCommit
            | Message::EditorTextCancel
            | Message::EditorWidthBump(..)
            | Message::EditorRotate { .. }
            | Message::EditorFlip(..)
            | Message::EditorResizeOpen
            | Message::EditorResizeWidthChanged(..)
            | Message::EditorResizeHeightChanged(..)
            | Message::EditorResizeUnitChanged(..)
            | Message::EditorResizeLockToggled(..)
            | Message::EditorResizeApply
            | Message::EditorCombineCaptureRequested(..)
            | Message::EditorAddImageFromFile
            | Message::EditorPartLoaded { .. }
            | Message::EditorLayoutGapPreview(..)
            | Message::EditorLayoutGapFlush { .. }
            | Message::EditorLayoutAlign(..)
            | Message::EditorComposed { .. }
            | Message::EditorColorCycle(..)
            | Message::EditorZoomIn
            | Message::EditorZoomOut
            | Message::EditorZoomActual
            | Message::EditorZoomFit
            | Message::EditorPinRequested
            | Message::EditorPinRendered { .. }
            | Message::PinWindowReady(..)
            | Message::PinClosePressed(..)
            | Message::PinDragRequested(..)
            | Message::PinOpacityChanged(..)
            | Message::PinLockToggled(..)
            | Message::PinHovered(..)
            | Message::PinKeyPressed { .. }
            | Message::EditorTextRecognize
            | Message::EditorTextRecognized { .. }
            | Message::EditorPanelToggled
            | Message::EditorPanelTabSelected(..)
            | Message::EditorPanelOverlayDismissed
            | Message::EditorOpenTextSettings
            | Message::EditorTextLineClicked(..)
            | Message::EditorCopyLines
            | Message::EditorCopyLinesDone(..)
            | Message::EditorRemoveLineBreaksToggled(..)
            | Message::EditorRedactStyleChanged(..)
            | Message::EditorRedactAll
            | Message::EditorRedactLine(..)
            | Message::EditorHistoryWritten { .. })
    };
}

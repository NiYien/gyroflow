// SPDX-License-Identifier: GPL-3.0-or-later

import QtQml

QtObject {
    id: root

    property var acceptedCallback: null
    property var rejectedCallback: null

    function begin(onAccepted: var, onRejected: var): void {
        root.acceptedCallback = onAccepted
        root.rejectedCallback = onRejected
    }

    function clear(): void {
        root.acceptedCallback = null
        root.rejectedCallback = null
    }

    function accept(value: var): void {
        const callback = root.acceptedCallback
        root.clear()
        if (callback) callback(value)
    }

    function reject(): void {
        const callback = root.rejectedCallback
        root.clear()
        if (callback) callback()
    }
}

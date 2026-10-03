// SPDX-License-Identifier: GPL-3.0-or-later
package com.niyien.stabilizer;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;

/** Decodes escaped volume descriptions for display, never filesystem paths. */
final class StorageVolumeLabel {
    private StorageVolumeLabel() {}

    static String displayName(String value) {
        if (value == null || !value.contains("M-")) return value;
        StringBuilder result = new StringBuilder();
        for (int i = 0; i < value.length();) {
            if (!value.startsWith("M-", i)) {
                result.append(value.charAt(i++));
                continue;
            }
            ByteArrayOutputStream bytes = new ByteArrayOutputStream();
            while (value.startsWith("M-", i)) {
                i += 2;
                if (i >= value.length()) return value;
                int ch = value.charAt(i++);
                if (ch == '^') {
                    if (i >= value.length()) return value;
                    ch = value.charAt(i++);
                    if (ch != '?' && (ch < '@' || ch > '_')) return value;
                    ch ^= 0x40;
                } else if (ch < 0x20 || ch > 0x7e) {
                    return value;
                }
                bytes.write(ch | 0x80);
            }
            try {
                String decoded = StandardCharsets.UTF_8.newDecoder()
                        .onMalformedInput(CodingErrorAction.REPORT)
                        .onUnmappableCharacter(CodingErrorAction.REPORT)
                        .decode(ByteBuffer.wrap(bytes.toByteArray())).toString();
                // Reject incomplete data and control characters instead of guessing.
                if (decoded.codePoints().anyMatch(Character::isISOControl)) return value;
                result.append(decoded);
            } catch (CharacterCodingException ignored) {
                return value;
            }
        }
        return result.toString();
    }
}

# SPDX-License-Identifier: GPL-3.0-or-later
"""Fill catalog gaps while preserving existing translations and file formatting."""
import argparse
import collections
import html
import json
import re
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DIRECTORY = ROOT / 'resources/translations'
DATA = DIRECTORY / 'coverage.json'
ENGLISH_FALLBACKS = {
    'The demo could not be prepared. Please try again.',
    'The output file is unavailable. Check the output folder.',
    'About NiYien',
    'Help and support',
    'Open-source licenses',
    'Privacy policy',
    'Share / Save to Files',
    'Source code',
    'Try a generated demo',
}


def message_key(context, message):
    return (context, message.findtext('source', ''), message.findtext('comment', ''))


def placeholders(text):
    return collections.Counter(re.findall(r'%(?:L?[1-9][0-9]*|Ln|n)', text))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-catalog', type=Path, required=True,
                        help='Fresh lupdate output for the current UI')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    source_root = ET.parse(args.source_catalog).getroot()
    active = {}
    for context in source_root.findall('context'):
        for message in context.findall('message'):
            for location in message.findall('location'):
                if not location.get('filename') or not location.get('line', '').isdigit():
                    raise ValueError('Use lupdate -locations absolute for the source catalog')
            active[message_key(context.findtext('name'), message)] = message
    data = json.loads(DATA.read_text(encoding='utf-8'))
    supplied = {}
    for source, row in data['messages'].items():
        columns = row.split('|')
        if len(columns) != len(data['languages']):
            raise ValueError('Wrong language count: ' + source)
        supplied[source] = dict(zip(data['languages'], columns))
    plans = []
    errors = []
    for language in data['languages']:
        path = DIRECTORY / (language + '.ts')
        original = path.read_bytes()
        raw = original.decode('utf-8')
        eol = '\r\n' if '\r\n' in raw else '\n'
        tree = ET.fromstring(raw)
        known = {}
        reusable = {}
        for context in tree.findall('context'):
            for message in context.findall('message'):
                key = message_key(context.findtext('name'), message)
                translation = message.find('translation')
                if key in known:
                    errors.append(f'{language}: duplicate message {key}')
                known[key] = translation
                if translation is not None and translation.get('type') not in ('unfinished', 'vanished', 'obsolete') and translation.text:
                    reusable.setdefault(key[1], translation.text)
        changes = {}
        for key in active:
            translation = known.get(key)
            text = translation.text if translation is not None else None
            fallback = key[1] in ENGLISH_FALLBACKS and text == key[1]
            needs = key not in known or translation is None or translation.get('type') in ('unfinished', 'vanished', 'obsolete') or not text or fallback
            if not needs:
                if placeholders(text) != placeholders(key[1]):
                    errors.append(f'{language}: placeholders differ {key}')
                continue
            replacement = supplied.get(key[1], {}).get(language)
            if replacement is None:
                replacement = reusable.get(key[1])
            if replacement is None:
                errors.append(f'{language}: missing {key}')
                continue
            if not replacement.strip() and key[1].strip():
                errors.append(f'{language}: empty translation {key}')
            if placeholders(replacement) != placeholders(key[1]):
                errors.append(f'{language}: placeholders differ {key}')
            changes[key] = replacement
        remaining = dict(changes)

        def patch_context(match):
            block = match.group(0)
            name = html.unescape(re.search(r'<name>(.*?)</name>', block, re.S).group(1))

            def patch_message(match):
                text = match.group(0)
                element = ET.fromstring(text)
                key = message_key(name, element)
                if key not in remaining:
                    return text
                old = element.find('translation')
                replacement = '<translation>' + html.escape(remaining.pop(key), quote=False).replace('\n', eol) + '</translation>'
                if old is None:
                    return text.replace('</message>', '    ' + replacement + eol + '    </message>')
                return re.sub(r'<translation\b[^>]*(?:/>|>.*?</translation>)', lambda _: replacement, text, count=1, flags=re.S)

            return re.sub(r'<message\b[^>]*>.*?</message>', patch_message, block, flags=re.S)

        raw = re.sub(r'<context>.*?</context>', patch_context, raw, flags=re.S)
        additions = collections.defaultdict(list)
        for key, replacement in remaining.items():
            element = ET.fromstring(ET.tostring(active[key], encoding='unicode'))
            for location in element.findall('location'):
                filename = location.get('filename')
                if filename:
                    location.set('filename', '../../src/ui/' + filename.split('/src/ui/', 1)[-1])
            element.find('translation').clear()
            element.find('translation').text = replacement
            ET.indent(element, space='    ', level=1)
            serialized = ET.tostring(element, encoding='unicode', short_empty_elements=False).strip()
            serialized = re.sub(r'<location([^>]*)></location>', r'<location\1/>', serialized)
            additions[key[0]].append('    ' + serialized + eol)
        for name, messages in additions.items():
            block = ''.join(messages).replace('\n', eol) if eol == '\n' else ''.join(messages).replace('\r\n', '\n').replace('\n', eol)
            pattern = r'(<context>\s*<name>' + re.escape(html.escape(name, quote=False)) + r'</name>.*?)(</context>)'
            if re.search(pattern, raw, re.S):
                raw = re.sub(pattern, lambda m: m[1] + block + m[2], raw, count=1, flags=re.S)
            else:
                raw = raw.replace('</TS>', '<context>' + eol + '    <name>' + html.escape(name, quote=False) + '</name>' + eol + block + '</context>' + eol + '</TS>')
        ET.fromstring(raw)
        plans.append((path, original, raw.encode('utf-8'), len(changes)))
    if errors:
        raise SystemExit('\n'.join(errors))
    if args.check:
        gaps = [(path.stem, count) for path, _, _, count in plans if count]
        if gaps:
            raise SystemExit('Remaining gaps: ' + repr(gaps))
        print(f'PASS: {len(active)} current messages covered in {len(plans)} languages')
        return
    for path, original, updated, count in plans:
        if path.read_bytes() != original:
            raise RuntimeError('File changed during update: ' + str(path))
        if updated != original:
            path.write_bytes(updated)
        print(f'{path.stem}: {count} translations completed')


if __name__ == '__main__':
    main()

#!/bin/sh
set -eu

domain=beam
canonical=beam-gtk/po/en-US.po

for catalog in en-US pt-BR es-ES zh-CN; do
    po="beam-gtk/po/$catalog.po"
    test -f "$po"
    msgfmt --check --check-format --check-header -o "/tmp/$domain-$catalog.mo" "$po"
    canonical_keys=$(mktemp)
    catalog_keys=$(mktemp)
    msgattrib --no-obsolete "$canonical" | sed -n '/^msgid /p' | sort > "$canonical_keys"
    msgattrib --no-obsolete "$po" | sed -n '/^msgid /p' | sort > "$catalog_keys"
    diff -u "$canonical_keys" "$catalog_keys"
    rm -f "$canonical_keys" "$catalog_keys"
    test "$(msgattrib --untranslated "$po" | sed -n 's/^msgid /x/p' | wc -l)" -eq 0
    test "$(msgattrib --only-fuzzy "$po" | sed -n 's/^msgid /x/p' | wc -l)" -eq 0
    # The sole repeated `msgid ""` is the PO header plus gettext's multiline
    # representation; any additional duplicate indicates a real repeated key.
    test "$(msgattrib --no-obsolete "$po" | sed -n 's/^msgid /x/p' | sort | uniq -d | wc -l)" -eq 1
done

# Every message used by the GTK frontend must exist in the canonical catalog.
missing_keys=$(mktemp)
rg -o 'gettext\("[^"\\]*"\)' beam-gtk/src --glob '*.rs' \
    | sed 's/.*gettext("//; s/")$//' | sort -u | while IFS= read -r message; do
    if ! grep -Fq "msgid \"$message\"" "$canonical"; then
        echo "missing canonical key: $message" >&2
        echo "$message" >> "$missing_keys"
    fi
done
# The same for the web interface: every tr("…")/trf("…") literal in ui/js.
rg -o '\btrf?\("[^"\\]*"' beam-gtk/ui/js --glob '*.js' \
    | sed 's/.*trf\?("//; s/"$//' | sort -u | while IFS= read -r message; do
    if ! grep -Fq "msgid \"$message\"" "$canonical"; then
        echo "missing canonical key (ui): $message" >&2
        echo "$message" >> "$missing_keys"
    fi
done
test ! -s "$missing_keys"
rm -f "$missing_keys"

# IDs, icon names, CSS classes, paths, technical diagnostics and product names are intentional.
if rg -n '\.(title|label|heading|description|tooltip_text)\("[[:alpha:]][^"]*"\)|set_tooltip_text\(Some\("' \
    beam-gtk/src --glob '*.rs' | grep -vE 'title\("Beam"\)|application_name\("Beam"\)|developer_name\("Lyra OS"\)'; then
    echo "untranslated user-facing literal found" >&2
    exit 1
fi

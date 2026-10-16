import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Button,
  Dropdown,
  Modal,
  OverflowMenu,
  OverflowMenuItem,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  Tag,
  Tile,
} from "@carbon/react";
import { TrashCan, Time } from "@carbon/icons-react";
import { api } from "@/tauri/api";
import type { ArchiveEntry } from "@/tauri/types";
import { filterHistory, formatTimestamp, historyEntryHasFile, type HistoryFilter } from "@/lib/history";
import { t } from "@/lib/i18n";
import { useToasts } from "@/state/useToasts";

interface HistoryTableProps {
  /** Bump to reload (e.g. after a job completes). */
  refreshKey: number;
  onRedownload: (url: string) => void;
}

const FILTERS: Array<{ id: HistoryFilter; label: string }> = [
  { id: "all", label: t("history.filter.all") },
  { id: "Completed", label: t("history.filter.completed") },
  { id: "Failed", label: t("history.filter.failed") },
];

export function HistoryTable({ refreshKey, onRedownload }: HistoryTableProps) {
  const toasts = useToasts();
  const [entries, setEntries] = useState<ArchiveEntry[]>([]);
  const [filter, setFilter] = useState(FILTERS[0]);
  const [confirmClear, setConfirmClear] = useState(false);

  const load = useCallback(async () => {
    try {
      setEntries(await api.listHistory());
    } catch (err) {
      toasts.error(t("history.loadFailed"), err);
    }
  }, [toasts]);

  useEffect(() => {
    void load();
  }, [load, refreshKey]);

  const visible = useMemo(() => filterHistory(entries, filter.id), [entries, filter]);

  const reveal = async (entry: ArchiveEntry) => {
    try {
      await api.revealOutput(entry.destination);
    } catch (err) {
      toasts.error(t("history.revealFailed"), err);
    }
  };
  const open = async (entry: ArchiveEntry) => {
    try {
      await api.openOutput(entry.destination);
    } catch (err) {
      toasts.error(t("history.openFailed"), err);
    }
  };
  const remove = async (entry: ArchiveEntry) => {
    try {
      await api.removeHistoryEntry(entry.media_id);
      await load();
    } catch (err) {
      toasts.error(t("history.removeFailed"), err);
    }
  };
  const clearAll = async () => {
    setConfirmClear(false);
    try {
      const outcome = filter.id === "all" ? null : filter.id;
      const removed = await api.clearHistory(outcome);
      toasts.success(t("history.cleared", { count: removed }));
      await load();
    } catch (err) {
      toasts.error(t("history.clearFailed"), err);
    }
  };

  return (
    <div className="ph-panel" data-testid="history-panel">
      <div style={{ display: "flex", alignItems: "flex-end", gap: "1rem", flexWrap: "wrap" }}>
        <Dropdown
          id="history-filter"
          size="md"
          titleText={t("history.filter.label")}
          label={t("history.filter.label")}
          items={FILTERS}
          itemToString={(i) => i?.label ?? ""}
          selectedItem={filter}
          onChange={({ selectedItem }) => selectedItem && setFilter(selectedItem)}
        />
        <span style={{ flex: 1 }} />
        <Button
          kind="danger--tertiary"
          size="md"
          renderIcon={TrashCan}
          disabled={visible.length === 0}
          onClick={() => setConfirmClear(true)}
        >
          {t("history.clear")}
        </Button>
      </div>

      {visible.length === 0 ? (
        <Tile className="ph-empty" data-testid="history-empty">
          <Time size={32} />
          <span>{filter.id === "Failed" ? t("history.emptyFailed") : t("history.empty")}</span>
        </Tile>
      ) : (
        <div className="ph-scroll">
          <Table size="md" aria-label={t("history.title")}>
            <TableHead>
              <TableRow>
                <TableHeader className="ph-history__col ph-history__col--title">{t("queue.col.title")}</TableHeader>
                <TableHeader className="ph-history__col ph-history__col--status">{t("queue.col.status")}</TableHeader>
                <TableHeader className="ph-history__col ph-history__col--when">{t("history.col.when")}</TableHeader>
                <TableHeader className="ph-history__col ph-history__col--actions" />
              </TableRow>
            </TableHead>
            <TableBody>
              {visible.map((entry) => (
                <TableRow key={entry.media_id} data-testid={`history-row-${entry.media_id}`}>
                  <TableCell className="ph-history__col ph-history__col--title">
                    <div className="ph-history__title" title={entry.title}>
                      {entry.title}
                    </div>
                    <div className="ph-queue__url" title={entry.url}>
                      {entry.url}
                    </div>
                  </TableCell>
                  <TableCell className="ph-history__col ph-history__col--status">
                    <Tag size="sm" type={entry.outcome === "Completed" ? "green" : "red"}>
                      {entry.outcome === "Completed" ? t("queue.status.completed") : t("queue.status.failed")}
                    </Tag>
                  </TableCell>
                  <TableCell className="ph-history__col ph-history__col--when">{formatTimestamp(entry.completed_at_unix_secs)}</TableCell>
                  <TableCell className="ph-history__col ph-history__col--actions">
                    <div style={{ display: "flex", justifyContent: "flex-end" }}>
                      <OverflowMenu size="sm" flipped aria-label={t("queue.rowMenu", { title: entry.title })} iconDescription={t("queue.rowMenu", { title: entry.title })}>
                        <OverflowMenuItem itemText={t("queue.action.reveal")} onClick={() => void reveal(entry)} />
                        <OverflowMenuItem itemText={t("queue.action.open")} disabled={!historyEntryHasFile(entry)} onClick={() => void open(entry)} />
                        <OverflowMenuItem itemText={t("history.redownload")} onClick={() => onRedownload(entry.url)} />
                        <OverflowMenuItem hasDivider isDelete itemText={t("history.remove")} onClick={() => void remove(entry)} />
                      </OverflowMenu>
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}

      {confirmClear && (
      <Modal
        open
        danger
        size="sm"
        modalHeading={t("history.clearConfirmTitle")}
        primaryButtonText={t("history.clear")}
        secondaryButtonText={t("discard.cancel")}
        onRequestClose={() => setConfirmClear(false)}
        onRequestSubmit={() => void clearAll()}
      >
        <p>{t("history.clearConfirmBody")}</p>
      </Modal>
      )}
    </div>
  );
}

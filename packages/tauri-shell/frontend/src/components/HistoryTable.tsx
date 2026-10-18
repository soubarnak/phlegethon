import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Button,
  Dropdown,
  Modal,
  OverflowMenu,
  OverflowMenuItem,
  Pagination,
  Search,
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
import {
  HISTORY_PAGE_SIZES,
  HISTORY_SORT_FINISHED,
  HISTORY_SORT_NAME,
  HISTORY_SORT_SIZE,
  applyHistoryView,
  formatTimestamp,
  historyEntryHasFile,
  paginateHistory,
  type HistoryFilter,
  type HistorySort,
  type HistorySortDir,
} from "@/lib/history";
import { formatBytes } from "@/lib/progress";
import { t } from "@/lib/i18n";
import { useToasts } from "@/state/useToasts";

interface HistoryTableProps {
  /** Bump to reload (e.g. after a job completes). */
  refreshKey: number;
  onRedownload: (url: string) => void;
}

// Built on first render, not at module scope: t() at import time runs before initLocale() (L-59).
const historyFilters = (): Array<{ id: HistoryFilter; label: string }> => [
  { id: "all", label: t("history.filter.all") },
  { id: "Completed", label: t("history.filter.completed") },
  { id: "Failed", label: t("history.filter.failed") },
];

interface HistorySortOption {
  id: string;
  label: string;
  sort: HistorySort;
  dir: HistorySortDir;
}

const historySorts = (): HistorySortOption[] => [
  { id: "finished-desc", label: t("history.sort.finishedDesc"), sort: HISTORY_SORT_FINISHED, dir: "desc" },
  { id: "finished-asc", label: t("history.sort.finishedAsc"), sort: HISTORY_SORT_FINISHED, dir: "asc" },
  { id: "name-asc", label: t("history.sort.nameAsc"), sort: HISTORY_SORT_NAME, dir: "asc" },
  { id: "name-desc", label: t("history.sort.nameDesc"), sort: HISTORY_SORT_NAME, dir: "desc" },
  { id: "size-desc", label: t("history.sort.sizeDesc"), sort: HISTORY_SORT_SIZE, dir: "desc" },
  { id: "size-asc", label: t("history.sort.sizeAsc"), sort: HISTORY_SORT_SIZE, dir: "asc" },
];

export function HistoryTable({ refreshKey, onRedownload }: HistoryTableProps) {
  const toasts = useToasts();
  const [entries, setEntries] = useState<ArchiveEntry[]>([]);
  const filters = useMemo(historyFilters, []);
  const sorts = useMemo(historySorts, []);
  const [filter, setFilter] = useState(filters[0]);
  const [sortOption, setSortOption] = useState(sorts[0]);
  const [search, setSearch] = useState("");
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(HISTORY_PAGE_SIZES[0]);
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

  const searching = search.trim() !== "";
  const visible = useMemo(
    () => applyHistoryView(entries, { search, filter: filter.id, sort: sortOption.sort, dir: sortOption.dir }),
    [entries, search, filter, sortOption],
  );
  // The page is clamped here, so a list that shrank after a remove or clear still shows its last page.
  const paged = useMemo(() => paginateHistory(visible, page, pageSize), [visible, page, pageSize]);

  // Any change to what matches starts again from the first page.
  const changeSearch = (value: string) => {
    setSearch(value);
    setPage(1);
  };
  const changeFilter = (option: (typeof filters)[number]) => {
    setFilter(option);
    setPage(1);
  };
  const changeSort = (option: HistorySortOption) => {
    setSortOption(option);
    setPage(1);
  };

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
      <div className="ph-history__toolbar">
        <Search
          id="history-search"
          size="md"
          labelText={t("history.search.label")}
          placeholder={t("history.search.placeholder")}
          closeButtonLabelText={t("history.search.clear")}
          value={search}
          onChange={(e) => changeSearch(e.target.value)}
          onClear={() => changeSearch("")}
        />
        <div className="ph-history__toolbar-controls">
          <Dropdown
            id="history-filter"
            size="md"
            titleText={t("history.filter.label")}
            label={t("history.filter.label")}
            items={filters}
            itemToString={(i) => i?.label ?? ""}
            selectedItem={filter}
            onChange={({ selectedItem }) => selectedItem && changeFilter(selectedItem)}
          />
          <Dropdown
            id="history-sort"
            size="md"
            titleText={t("history.sort.label")}
            label={t("history.sort.label")}
            items={sorts}
            itemToString={(i) => i?.label ?? ""}
            selectedItem={sortOption}
            onChange={({ selectedItem }) => selectedItem && changeSort(selectedItem)}
          />
        </div>
        <div className="ph-history__clear">
          {searching && <span className="ph-history__hint">{t("history.clearSearchHint")}</span>}
          <Button
            kind="danger--tertiary"
            size="md"
            renderIcon={TrashCan}
            disabled={searching || visible.length === 0}
            onClick={() => setConfirmClear(true)}
          >
            {t("history.clear")}
          </Button>
        </div>
      </div>

      {visible.length === 0 ? (
        <Tile className="ph-empty" data-testid="history-empty">
          <Time size={32} />
          <span>
            {searching
              ? t("history.emptySearch")
              : filter.id === "Failed"
                ? t("history.emptyFailed")
                : t("history.empty")}
          </span>
        </Tile>
      ) : (
        <>
          <div className="ph-scroll">
            <Table size="md" aria-label={t("history.title")}>
              <TableHead>
                <TableRow>
                  <TableHeader className="ph-history__col ph-history__col--title">{t("queue.col.title")}</TableHeader>
                  <TableHeader className="ph-history__col ph-history__col--status">{t("queue.col.status")}</TableHeader>
                  <TableHeader className="ph-history__col ph-history__col--when">{t("history.col.when")}</TableHeader>
                  <TableHeader className="ph-history__col ph-history__col--size">{t("history.col.size")}</TableHeader>
                  <TableHeader className="ph-history__col ph-history__col--actions">
                    <span className="cds--visually-hidden">{t("queue.col.actions")}</span>
                  </TableHeader>
                </TableRow>
              </TableHead>
              <TableBody>
                {paged.items.map((entry) => (
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
                    <TableCell className="ph-history__col ph-history__col--size">{formatBytes(entry.size_bytes)}</TableCell>
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
          <Pagination
            className="ph-history__pagination"
            size="md"
            page={paged.page}
            pageSize={pageSize}
            pageSizes={HISTORY_PAGE_SIZES}
            totalItems={visible.length}
            onChange={({ page: nextPage, pageSize: nextSize }) => {
              setPage(nextPage);
              setPageSize(nextSize);
            }}
            itemsPerPageText={t("history.page.perPage")}
            backwardText={t("history.page.backward")}
            forwardText={t("history.page.forward")}
            itemRangeText={(min, max, total) => t("history.page.range", { min, max, total })}
            itemText={(min, max) => t("history.page.items", { min, max })}
            pageRangeText={(_current, total) => t("history.page.of", { total })}
            pageText={(current) => t("history.page.current", { page: current })}
            pageSelectLabelText={(total) => t("history.page.select", { total })}
          />
        </>
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

import { memo, useMemo, useState } from "react";
import {
  Button,
  DataTable,
  Dropdown,
  OverflowMenu,
  OverflowMenuItem,
  ProgressBar,
  Table,
  TableBatchAction,
  TableBatchActions,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableHeader,
  TableRow,
  TableSelectAll,
  TableSelectRow,
  TableToolbar,
  TableToolbarContent,
  TableToolbarSearch,
  Tag,
  Tile,
} from "@carbon/react";
import { Pause, Play, Renew, TrashCan, Download } from "@carbon/icons-react";
import type { JobRow } from "@/state/useJobs";
import type { ExtractionTier, JobStatus } from "@/tauri/types";
import { t } from "@/lib/i18n";
import {
  QUEUE_FILTER_ALL,
  QUEUE_SORT_QUEUE,
  QUEUE_SORT_SIZE,
  QUEUE_SORT_STATUS,
  QUEUE_SORT_TITLE,
  applyQueueView,
} from "@/lib/queueView";
import { displayProgressPercent, estimateEtaSeconds, formatBytes, formatEta, formatRate } from "@/lib/progress";
import { errorHint, parseCoreError } from "@/lib/errors";
import { NARROW_QUERY, useMediaQuery } from "@/state/useMediaQuery";

export interface QueueActions {
  pause: (jobId: string) => void;
  resume: (jobId: string) => void;
  retry: (jobId: string) => void;
  discard: (jobIds: string[]) => void;
  moveUp: (job: JobRow) => void;
  moveDown: (job: JobRow) => void;
  reveal: (job: JobRow) => void;
  open: (job: JobRow) => void;
  copyUrl: (job: JobRow) => void;
  addMirror: (job: JobRow) => void;
  setJobCap: (job: JobRow) => void;
  pauseMany: (jobIds: string[]) => void;
  resumeMany: (jobIds: string[]) => void;
  retryMany: (jobIds: string[]) => void;
}

interface QueueTableProps {
  jobs: JobRow[];
  actions: QueueActions;
  hasAnyJobs: boolean;
}

// Built on first render, not at module scope: t() at import time runs before initLocale() (L-59).
const queueHeaders = () => [
  { key: "title", header: t("queue.col.title") },
  { key: "status", header: t("queue.col.status") },
  { key: "progress", header: t("queue.col.progress") },
  { key: "size", header: t("queue.col.size") },
  { key: "actions", header: t("queue.col.actions") },
];

/** Column key to cell class: lets the stylesheet size, hide or restack columns by width (UI-1). */
const colClass = (key: string) => `ph-queue__col ph-queue__col--${key}`;

type StatusTagType = "gray" | "blue" | "teal" | "green" | "red";

const STATUS_TAG: Record<JobStatus, StatusTagType> = {
  pending: "gray",
  downloading: "blue",
  paused: "teal",
  completed: "green",
  failed: "red",
};

function statusLabel(status: JobStatus): string {
  switch (status) {
    case "pending":
      return t("queue.status.queued");
    case "downloading":
      return t("queue.status.downloading");
    case "paused":
      return t("queue.status.paused");
    case "completed":
      return t("queue.status.completed");
    case "failed":
      return t("queue.status.failed");
  }
}

function JobError({ raw }: { raw: string }) {
  const parsed = parseCoreError(raw);
  const hint = errorHint(parsed.code);
  return (
    <span className="ph-queue__error" data-testid="job-error" title={raw}>
      {parsed.message}
      {hint && <span className="ph-queue__hint">{hint}</span>}
    </span>
  );
}

function tierTag(tier: ExtractionTier | null) {
  switch (tier) {
    case "generic_extractor":
      return (
        <Tag size="sm" type="warm-gray" title={t("queue.tier.genericHint")}>
          {t("queue.tier.generic")}
        </Tag>
      );
    case "page_sniff":
      return (
        <Tag size="sm" type="purple" title={t("queue.tier.sniffHint")}>
          {t("queue.tier.sniff")}
        </Tag>
      );
    case "direct_file":
      return (
        <Tag size="sm" type="cool-gray">
          {t("queue.tier.file")}
        </Tag>
      );
    default:
      return null;
  }
}

interface QueueRowCellsProps {
  job: JobRow;
  actions: QueueActions;
  isLast: boolean;
}

/**
 * The cells of one queue row. Memoized (H-25): useJobs keeps a job's object identity until its
 * fields change, so a progress tick on one job re-renders only that job's cells.
 */
const QueueRowCells = memo(function QueueRowCells({ job, actions, isLast }: QueueRowCellsProps) {
  const pct = displayProgressPercent(job.bytes_downloaded, job.total_bytes);
  const rate = job.status === "downloading" ? formatRate(job.speed_bps) : "";
  const eta = job.status === "downloading" ? formatEta(estimateEtaSeconds(job.bytes_downloaded, job.total_bytes, job.speed_bps)) : "";
  const helper = [
    job.total_bytes != null
      ? `${formatBytes(job.bytes_downloaded)} / ${formatBytes(job.total_bytes)}`
      : job.bytes_downloaded > 0
        ? formatBytes(job.bytes_downloaded)
        : "",
    rate,
    eta ? t("queue.eta", { eta }) : "",
  ]
    .filter(Boolean)
    .join(" · ");
  const progressStatus = job.status === "completed" ? "finished" : job.status === "failed" ? "error" : "active";
  const canReveal = job.status === "completed" && !!job.path;
  const isFirst = job.queue_index === 0;
  return (
    <>
        <TableCell className={colClass("title")}>
          <div className="ph-queue__title">
            <span className="ph-queue__title-text" title={job.title}>
              {job.title}
            </span>
            <span className="ph-queue__url" title={job.url}>
              {job.url}
            </span>
            {job.status === "failed" && job.last_error && (
              <JobError raw={job.last_error} />
            )}
          </div>
        </TableCell>
        <TableCell className={colClass("status")}>
          <div className="ph-queue__tags">
            <Tag size="sm" type={STATUS_TAG[job.status]} data-testid="status-tag">
              {statusLabel(job.status)}
            </Tag>
            {tierTag(job.extraction_tier)}
          </div>
        </TableCell>
        <TableCell className={colClass("progress")}>
          <div className="ph-queue__progress">
            <ProgressBar
              size="small"
              hideLabel
              label={t("queue.progressFor", { title: job.title })}
              helperText={helper || (job.status === "pending" ? t("queue.status.queued") : "")}
              value={job.status === "completed" ? 100 : job.status === "pending" ? 0 : pct ?? undefined}
              max={100}
              status={progressStatus}
            />
          </div>
        </TableCell>
        <TableCell className={colClass("size")}>{job.total_bytes != null ? formatBytes(job.total_bytes) : "—"}</TableCell>
        <TableCell className={colClass("actions")}>
          <div style={{ display: "flex", justifyContent: "flex-end", gap: "0.25rem" }}>
            {(job.status === "pending" || job.status === "downloading") && (
              <Button kind="ghost" size="sm" hasIconOnly renderIcon={Pause} iconDescription={t("queue.action.pause")} onClick={() => actions.pause(job.job_id)} />
            )}
            {job.status === "paused" && (
              <Button kind="ghost" size="sm" hasIconOnly renderIcon={Play} iconDescription={t("queue.action.resume")} onClick={() => actions.resume(job.job_id)} />
            )}
            {job.status === "failed" && (
              <Button kind="ghost" size="sm" hasIconOnly renderIcon={Renew} iconDescription={t("queue.action.retry")} onClick={() => actions.retry(job.job_id)} />
            )}
            <OverflowMenu size="sm" flipped aria-label={t("queue.rowMenu", { title: job.title })} iconDescription={t("queue.rowMenu", { title: job.title })}>
              <OverflowMenuItem itemText={t("queue.action.moveUp")} disabled={isFirst} onClick={() => actions.moveUp(job)} />
              <OverflowMenuItem itemText={t("queue.action.moveDown")} disabled={isLast} onClick={() => actions.moveDown(job)} />
              <OverflowMenuItem itemText={t("queue.action.reveal")} disabled={!canReveal} onClick={() => actions.reveal(job)} />
              <OverflowMenuItem itemText={t("queue.action.open")} disabled={!canReveal} onClick={() => actions.open(job)} />
              <OverflowMenuItem itemText={t("queue.action.copyUrl")} onClick={() => actions.copyUrl(job)} />
              <OverflowMenuItem itemText={t("queue.action.addMirror")} disabled={job.is_extractor || job.status === "completed"} onClick={() => actions.addMirror(job)} />
              <OverflowMenuItem itemText={t("queue.action.bandwidthCap")} disabled={job.status === "completed"} onClick={() => actions.setJobCap(job)} />
              <OverflowMenuItem hasDivider isDelete itemText={t("queue.action.discard")} onClick={() => actions.discard([job.job_id])} />
            </OverflowMenu>
          </div>
        </TableCell>
    </>
        );
});

const queueFilters = () => [
  { id: QUEUE_FILTER_ALL, label: t("queue.filter.all") },
  { id: "queued", label: t("queue.filter.queued") },
  { id: "downloading", label: t("queue.filter.downloading") },
  { id: "paused", label: t("queue.filter.paused") },
  { id: "completed", label: t("queue.filter.completed") },
  { id: "failed", label: t("queue.filter.failed") },
];

const queueSorts = () => [
  { id: QUEUE_SORT_QUEUE, label: t("queue.sort.queue") },
  { id: QUEUE_SORT_TITLE, label: t("queue.sort.title") },
  { id: QUEUE_SORT_STATUS, label: t("queue.sort.status") },
  { id: QUEUE_SORT_SIZE, label: t("queue.sort.size") },
];

export function QueueTable({ jobs, actions, hasAnyJobs }: QueueTableProps) {
  const [search, setSearch] = useState("");
  const headers = useMemo(queueHeaders, []);
  const filters = useMemo(queueFilters, []);
  const sorts = useMemo(queueSorts, []);
  const [filter, setFilter] = useState(filters[0]);
  const [sort, setSort] = useState(sorts[0]);
  const narrow = useMediaQuery(NARROW_QUERY);

  const visible = useMemo(
    () => applyQueueView(jobs, { search, stateFilter: filter.id, sort: sort.id }),
    [jobs, search, filter, sort],
  );
  const byId = useMemo(() => new Map(visible.map((j) => [j.job_id, j])), [visible]);
  const rows = useMemo(
    () =>
      visible.map((j) => ({
        id: j.job_id,
        title: j.title,
        status: j.status,
        progress: j.bytes_downloaded,
        size: j.total_bytes ?? -1,
        actions: "",
      })),
    [visible],
  );

  if (!hasAnyJobs) {
    return (
      <Tile className="ph-empty" data-testid="queue-empty">
        <Download size={32} />
        <span>{t("queue.emptyWithHint")}</span>
      </Tile>
    );
  }

  return (
    <div className="ph-queue">
      <DataTable rows={rows} headers={headers} size="md" isSortable={false}>
        {({
          rows: tableRows,
          headers: tableHeaders,
          getHeaderProps,
          getRowProps,
          getSelectionProps,
          getBatchActionProps,
          getTableProps,
          getTableContainerProps,
          getToolbarProps,
          selectedRows,
        }) => {
          // Only rows still in the filtered view count: Carbon may keep a selection for a row the
          // filter has hidden, and Discard deletes files (M-68).
          const selectedJobs = selectedRows.map((r) => byId.get(r.id)).filter((j): j is JobRow => !!j);
          const selectedIds = selectedJobs.map((j) => j.job_id);
          const batchProps = getBatchActionProps();
          // Below the narrow breakpoint the labelled buttons no longer fit the bar, so show icons with tooltips (UI-4).
          const batchButton = (label: string) => (narrow ? { hasIconOnly: true, iconDescription: label } : {});
          // ...and the summary shrinks to the bare count so Cancel still fits beside them.
          const batchSummary = narrow
            ? {
                translateWithId: (id: string, values?: { totalSelected?: number }) =>
                  id === "carbon.table.batch.cancel" ? t("queue.batch.cancel") : String(values?.totalSelected ?? ""),
              }
            : {};
          return (
            <TableContainer {...getTableContainerProps()} title={t("queue.title")} description={t("queue.description", { count: jobs.length })}>
              <TableToolbar {...getToolbarProps()} aria-label={t("queue.toolbar")}>
                <TableBatchActions {...batchProps} {...batchSummary}>
                  <TableBatchAction
                    renderIcon={Pause}
                    {...batchButton(t("queue.action.pause"))}
                    tabIndex={batchProps.shouldShowBatchActions ? 0 : -1}
                    disabled={!selectedJobs.some((j) => j.status === "pending" || j.status === "downloading")}
                    onClick={() => actions.pauseMany(selectedJobs.filter((j) => j.status === "pending" || j.status === "downloading").map((j) => j.job_id))}
                  >
                    {t("queue.action.pause")}
                  </TableBatchAction>
                  <TableBatchAction
                    renderIcon={Play}
                    {...batchButton(t("queue.action.resume"))}
                    tabIndex={batchProps.shouldShowBatchActions ? 0 : -1}
                    disabled={!selectedJobs.some((j) => j.status === "paused")}
                    onClick={() => actions.resumeMany(selectedJobs.filter((j) => j.status === "paused").map((j) => j.job_id))}
                  >
                    {t("queue.action.resume")}
                  </TableBatchAction>
                  <TableBatchAction
                    renderIcon={Renew}
                    {...batchButton(t("queue.action.retry"))}
                    tabIndex={batchProps.shouldShowBatchActions ? 0 : -1}
                    disabled={!selectedJobs.some((j) => j.status === "failed")}
                    onClick={() => actions.retryMany(selectedJobs.filter((j) => j.status === "failed").map((j) => j.job_id))}
                  >
                    {t("queue.action.retry")}
                  </TableBatchAction>
                  <TableBatchAction
                    renderIcon={TrashCan}
                    {...batchButton(t("queue.action.discard"))}
                    tabIndex={batchProps.shouldShowBatchActions ? 0 : -1}
                    onClick={() => actions.discard(selectedIds)}
                  >
                    {t("queue.action.discard")}
                  </TableBatchAction>
                </TableBatchActions>
                <TableToolbarContent aria-hidden={batchProps.shouldShowBatchActions}>
                  <TableToolbarSearch
                    persistent
                    placeholder={t("queue.search.placeholder")}
                    labelText={t("queue.search.label")}
                    onChange={(e) => setSearch(typeof e === "string" ? e : (e?.target as HTMLInputElement)?.value ?? "")}
                    tabIndex={batchProps.shouldShowBatchActions ? -1 : 0}
                  />
                  <div className="ph-queue__toolbar-controls">
                    <Dropdown
                      id="queue-filter"
                      size="md"
                      hideLabel
                      titleText={t("queue.filter.label")}
                      label={t("queue.filter.label")}
                      items={filters}
                      itemToString={(i) => i?.label ?? ""}
                      selectedItem={filter}
                      onChange={({ selectedItem }) => selectedItem && setFilter(selectedItem)}
                    />
                    <Dropdown
                      id="queue-sort"
                      size="md"
                      hideLabel
                      titleText={t("queue.sort.label")}
                      label={t("queue.sort.label")}
                      items={sorts}
                      itemToString={(i) => i?.label ?? ""}
                      selectedItem={sort}
                      onChange={({ selectedItem }) => selectedItem && setSort(selectedItem)}
                    />
                  </div>
                </TableToolbarContent>
              </TableToolbar>
              {visible.length === 0 ? (
                <div className="ph-empty">{t("queue.emptyFiltered")}</div>
              ) : (
                <Table {...getTableProps()} aria-label={t("queue.title")}>
                  <TableHead>
                    <TableRow>
                      <TableSelectAll {...getSelectionProps()} />
                      {tableHeaders.map((header) => {
                        const { key, ...headerProps } = getHeaderProps({ header });
                        return (
                          <TableHeader key={key} {...headerProps} className={colClass(header.key)}>
                            {header.key === "actions" ? <span className="cds--visually-hidden">{header.header}</span> : header.header}
                          </TableHeader>
                        );
                      })}
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {tableRows.map((row) => {
                      const job = byId.get(row.id);
                      if (!job) return null;
                      const { key, ...rowProps } = getRowProps({ row });
                      return (
                        <TableRow key={key} {...rowProps} data-testid={`queue-row-${job.job_id}`}>
                          <TableSelectRow {...getSelectionProps({ row })} />
                          <QueueRowCells job={job} actions={actions} isLast={job.queue_index === jobs.length - 1} />
                        </TableRow>
                      );
                    })}
                  </TableBody>
                </Table>
              )}
            </TableContainer>
          );
        }}
      </DataTable>
    </div>
  );
}

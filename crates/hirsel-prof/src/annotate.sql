WITH matching_functions AS (
    SELECT id FROM function WHERE def_path=?1 OR instance_label=?1 OR name=?1
), cpu_events AS (
    SELECT file, line, event, u64_sum(self_total) AS total, u64_sum(sample_count) AS samples
    FROM line_cpu_summary
    WHERE (?2 IS NOT NULL AND file=?1 AND line=?2)
       OR (?2 IS NULL AND function_id IN matching_functions)
    GROUP BY file, line, event
), cpu_lines AS (
    SELECT file, line, json_group_array(json_object(
        'event', event,
        'self_total', CASE WHEN typeof(total)='text' OR total>9007199254740992
                           THEN CAST(total AS TEXT) ELSE total END,
        'sample_count', CASE WHEN typeof(samples)='text' OR samples>9007199254740992
                             THEN CAST(samples AS TEXT) ELSE samples END
    ) ORDER BY event) AS events
    FROM cpu_events GROUP BY file, line
), remark_counts AS (
    SELECT file, line, kind, outcome, evidence_level, u64_sum(count) AS total
    FROM codegen_remark
    WHERE (?2 IS NOT NULL AND file=?1 AND line=?2)
       OR (?2 IS NULL AND function_id IN matching_functions)
    GROUP BY file, line, kind, outcome, evidence_level
), remark_lines AS (
    SELECT file, line, json_group_array(json_object(
        'kind', kind, 'outcome', outcome, 'evidence_level', evidence_level,
        'count', CASE WHEN typeof(total)='text' OR total>9007199254740992
                      THEN CAST(total AS TEXT) ELSE total END
    ) ORDER BY kind, outcome, evidence_level) AS remarks
    FROM remark_counts GROUP BY file, line
), allocation_counts AS (
    SELECT f.file, f.line, a.allocation_kind,
        u64_sum(a.observed_count) AS count, u64_sum(a.observed_bytes) AS bytes,
        sum(a.estimated_count) AS estimated_count, sum(a.estimated_bytes) AS estimated_bytes
    FROM mem_alloc_agg a JOIN callsite c ON c.id=a.callsite_id
    JOIN frame f ON f.id=c.frame_id
    WHERE (?2 IS NOT NULL AND f.file=?1 AND f.line=?2)
       OR (?2 IS NULL AND f.function_id IN matching_functions AND f.file IS NOT NULL AND f.line IS NOT NULL)
    GROUP BY f.file, f.line, a.allocation_kind
), allocation_lines AS (
    SELECT file, line, json_group_array(json_object(
        'allocation_kind', allocation_kind,
        'observed_count', CASE WHEN typeof(count)='text' OR count>9007199254740992
                               THEN CAST(count AS TEXT) ELSE count END,
        'observed_bytes', CASE WHEN typeof(bytes)='text' OR bytes>9007199254740992
                               THEN CAST(bytes AS TEXT) ELSE bytes END,
        'estimated_count', estimated_count, 'estimated_bytes', estimated_bytes
    ) ORDER BY allocation_kind) AS allocations
    FROM allocation_counts GROUP BY file, line
), lines AS (
    SELECT file, line FROM cpu_lines
    UNION SELECT file, line FROM remark_lines
    UNION SELECT file, line FROM allocation_lines
)
SELECT file, line,
       (SELECT sn.text FROM source_snippet sn
        WHERE sn.file=lines.file AND sn.start_line<=lines.line AND sn.end_line>=lines.line
        ORDER BY sn.end_line-sn.start_line, sn.start_line LIMIT 1),
       coalesce(cpu_lines.events, '[]'), coalesce(remark_lines.remarks, '[]'),
       coalesce(allocation_lines.allocations, '[]')
FROM lines
LEFT JOIN cpu_lines USING(file, line)
LEFT JOIN remark_lines USING(file, line)
LEFT JOIN allocation_lines USING(file, line)
ORDER BY file, line LIMIT ?3;

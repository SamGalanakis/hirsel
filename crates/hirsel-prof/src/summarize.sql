-- Group weights once per leaf, then reuse them across its distinct functions.
WITH RECURSIVE
leaf_weight(callsite_id, event, weight, samples) AS (
    SELECT s.callsite_id, c.event, u64_sum(c.value), count(*)
    FROM cpu_sample s JOIN sample_counter c ON c.sample_id = s.id
    GROUP BY s.callsite_id, c.event
),
ancestors(leaf_id, site_id) AS (
    SELECT DISTINCT callsite_id, callsite_id FROM leaf_weight
    UNION ALL
    SELECT a.leaf_id, s.parent_id
    FROM ancestors a JOIN callsite s ON s.id = a.site_id
    WHERE s.parent_id IS NOT NULL
),
membership(leaf_id, function_id, is_self) AS (
    SELECT a.leaf_id, f.function_id, max(a.leaf_id = a.site_id)
    FROM ancestors a JOIN callsite s ON s.id = a.site_id JOIN frame f ON f.id = s.frame_id
    WHERE f.function_id IS NOT NULL
    GROUP BY a.leaf_id, f.function_id
)
INSERT INTO fn_cpu_summary (function_id, event, self_total, inclusive_total, sample_count)
SELECT m.function_id, w.event, u64_sum(CASE WHEN m.is_self THEN w.weight ELSE 0 END),
    u64_sum(w.weight), u64_sum(w.samples)
FROM membership m JOIN leaf_weight w ON w.callsite_id = m.leaf_id
GROUP BY m.function_id, w.event;

INSERT INTO line_cpu_summary (file, line, function_id, event, self_total, sample_count)
SELECT f.file, f.line, f.function_id, c.event, u64_sum(c.value), count(*)
FROM cpu_sample s JOIN sample_counter c ON c.sample_id = s.id
JOIN callsite site ON site.id = s.callsite_id JOIN frame f ON f.id = site.frame_id
WHERE f.file IS NOT NULL AND f.line IS NOT NULL AND f.function_id IS NOT NULL
GROUP BY f.file, f.line, f.function_id, c.event;

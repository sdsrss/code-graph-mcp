-- synthetic import targets (non-<module>) that are nested inside another non-module node of the same file
select tf.path, tn.type, tn.name, tn.start_line, (select o.type||':'||o.name||'@'||o.start_line from nodes o where o.file_id=tn.file_id and o.id<>tn.id and o.type<>'module' and o.start_line<=tn.start_line and o.end_line>=tn.end_line order by o.start_line desc limit 1) as outer,
  (select count(*) from edges c join nodes cs on cs.id=c.source_id where c.relation='calls' and c.target_id=tn.id and c.metadata like '%"attr":%' and cs.file_id=sn.file_id and json_extract(c.metadata,'$.attr')=tn.name) as attr_calls,
  sf.path
from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
where e.relation='imports' and e.metadata like '%"attr":%' and tn.name<>'<module>'
  and exists (select 1 from nodes o where o.file_id=tn.file_id and o.id<>tn.id and o.type<>'module' and o.start_line<=tn.start_line and o.end_line>=tn.end_line);

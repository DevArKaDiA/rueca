create table audit_log (
  id number(10) not null,
  message varchar2(200),
  logged_at date default sysdate
) pctfree 10 nologging;

create sequence audit_seq start with 1;

create or replace procedure write_audit(p_message in varchar2) as
begin
  insert into audit_log (id, message) values (audit_seq.nextval, p_message);
  commit;
end write_audit;
/

create or replace procedure rotate_audit(p_days in number) as
  l_removed pls_integer := 0;
begin
  for r in (select id from audit_log where logged_at < sysdate - p_days) loop
    delete from audit_log where id = r.id;
    l_removed := l_removed + 1;
  end loop;
  write_audit('removed ' || l_removed);
exception
  when others then
    write_audit('rotation failed');
    raise;
end;
/

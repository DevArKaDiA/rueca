with customers as (
    select * from {{ ref('stg_customers') }}
)

select count(*) as n from customers

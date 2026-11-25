{{ config(materialized='table') }}

with orders as (
    select * from {{ ref('stg_orders') }}
)

select customer_id, count(*) as n, {{ cents_to_dollars('sum(cents)') }} as total
from orders
group by customer_id

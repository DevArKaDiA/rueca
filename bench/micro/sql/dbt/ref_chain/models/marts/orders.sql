{{ config(materialized='table') }}

with orders as (
    select * from {{ ref('stg_orders') }}
),
payments as (
    select * from {{ ref('stg_payments') }}
)

select orders.order_id, sum(payments.amount) as amount
from orders
left join payments on payments.order_id = orders.order_id
group by 1

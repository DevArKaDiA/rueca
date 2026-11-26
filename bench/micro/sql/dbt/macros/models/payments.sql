select
    id as payment_id,
    {{ dollars_with_tax('amount') }} as amount
from {{ ref('stg_payments') }}

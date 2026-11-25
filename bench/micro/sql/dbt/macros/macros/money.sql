{% macro cents_to_dollars(column_name, scale=2) %}
    ({{ column_name }} / 100)::numeric(16, {{ scale }})
{% endmacro %}

{% macro dollars_with_tax(column_name) %}
    {{ cents_to_dollars(column_name) }} * 1.2
{% endmacro %}

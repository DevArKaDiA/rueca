{% macro cents_to_dollars(expr) %}
    ({{ expr }} / 100.0)
{% endmacro %}

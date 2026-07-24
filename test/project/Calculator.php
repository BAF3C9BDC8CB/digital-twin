<?php
namespace App\Util;

/**
 * Simple calculator service.
 */
class Calculator {

    /**
     * Add two numbers together.
     */
    public function add(float $a, float $b): float {
        return $a + $b;
    }

    /**
     * Subtract b from a.
     */
    public function subtract(float $a, float $b): float {
        return $a - $b;
    }
}

/**
 * Math utility functions.
 */
function pi(): float {
    return 3.14159;
}
